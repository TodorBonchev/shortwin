//! Lender pool plumbing (ARCH §3.0, §3.3; ECON §9; `sim/pool_model.py` v0.4.2 through
//! `shortwin_math::pool`).
//!
//! The six lender instructions are thin wrappers: they load the math state
//! ([`Market::pool`], [`crate::state::MarketVault::vault`] (boxed, 2 KB),
//! [`LenderPosition::lender`]), call `shortwin_math::pool`, store the result back
//! ([`Market::set_pool`], [`crate::state::MarketVault::set_vault`],
//! [`LenderPosition::set_lender`]) and then move tokens or lamports. This module holds what
//! they share:
//!
//! - the market prologue that `deposit` and `withdraw` run first (ARCH §3.0 steps 1–5, A16) is
//!   [`crate::engine::prologue`]'s;
//! - [`load_records`]: the `ShareEpoch` records a lender is behind on, from
//!   `remaining_accounts`, key-checked against `["share_epoch", market, epoch u32 LE]` with
//!   the record's stored bump (`create_program_address`, no bump search; A39).
//! - [`store_records`]: writes back the conversion remainder `conv_rem` of every record the
//!   lender converted through (KI-65: cumulative rebase rounding, no orphan shares); such a
//!   record must have been passed writable.
//! - [`reads_rebase`]: whether a conversion read a rebase record, which `sync_lender` lets
//!   only the position's owner do (KI65-REV-SEC1; anyone else's sync is the pot-only touch,
//!   KI-66, and `RebaseNeedsOwner` its post-condition).
//! - [`init_if_new`]: the fields of a `LenderPosition` that `deposit` just created.
//! - [`rewards_required`]: which sweeps a deposit into a pool without shares waits for (a live
//!   wipe record, R4A-7; a market every lender left, AIR-08; on every market with the trader's
//!   pump UVA, AIR-05).
//! - [`pay_sol`], [`check_vault_floor`]: lender SOL out of `MarketVault`, never below its I2
//!   floor.
//!
//! The math crate's functions are atomic (a refusal changes nothing) and the handlers store
//! only after every check passed, so a failing instruction writes nothing even before the
//! runtime reverts it.

use anchor_lang::prelude::*;
use anchor_lang::AccountDeserialize;
use shortwin_math::pool::{Lender, Pool, ShareEpoch as MathShareEpoch, Vault};

use crate::errors::{CheckedExt, ShortwinError};
use crate::lamports;
use crate::state::{LenderPosition, Market, ShareEpoch, VenueKind};

/// The `ShareEpoch` records among the first `limit` of `accounts`, in order, as math records.
///
/// Each must be a short.win `ShareEpoch` of `market` at the address
/// `create_program_address(["share_epoch", market, epoch u32 LE, bump])` it claims, and the
/// records must be consecutive by epoch; otherwise `EpochRecordMismatch`. Which epochs are
/// needed is `pool::touch`'s business: records it does not need are ignored, missing ones
/// give `LenderBehindEpochs`.
pub fn load_records(
    market: &Pubkey,
    accounts: &[AccountInfo],
    limit: usize,
) -> Result<Vec<MathShareEpoch>> {
    let n = accounts.len().min(limit);
    let mut out: Vec<MathShareEpoch> = Vec::with_capacity(n);
    for info in accounts.iter().take(n) {
        require_keys_eq!(*info.owner, crate::ID, ShortwinError::EpochRecordMismatch);
        let rec = {
            let data = info.try_borrow_data()?;
            ShareEpoch::try_deserialize(&mut &data[..])
                .map_err(|_| error!(ShortwinError::EpochRecordMismatch))?
        };
        require_keys_eq!(rec.market, *market, ShortwinError::EpochRecordMismatch);
        let epoch_le = rec.epoch.to_le_bytes();
        let expected = Pubkey::create_program_address(
            &[ShareEpoch::SEED, market.as_ref(), &epoch_le, &[rec.bump]],
            &crate::ID,
        )
        .map_err(|_| error!(ShortwinError::EpochRecordMismatch))?;
        require_keys_eq!(info.key(), expected, ShortwinError::EpochRecordMismatch);
        if let Some(prev) = out.last() {
            require!(
                prev.epoch.checked_add(1) == Some(rec.epoch),
                ShortwinError::EpochRecordMismatch
            );
        }
        out.push(rec.record());
    }
    Ok(out)
}

/// Writes back the `conv_rem` of every record the lender's conversion changed (KI-65,
/// ECON §9.5): `before` is what [`load_records`] returned for the first `before.len()` of
/// `accounts`, `after` the same records after the pool calls. A changed record must be
/// writable (`EpochRecordReadOnly`); unchanged records may be read-only and are not
/// touched. [`load_records`] already checked each account's owner, discriminator, market
/// and address, so the 16 bytes at [`ShareEpoch::CONV_REM_OFFSET`] are written in place
/// (no Borsh round trip). Call it with the other stores, after every check passed.
pub fn store_records(
    accounts: &[AccountInfo],
    before: &[MathShareEpoch],
    after: &[MathShareEpoch],
) -> Result<()> {
    require_eq!(
        before.len(),
        after.len(),
        ShortwinError::EpochRecordMismatch
    );
    for ((info, b), a) in accounts.iter().zip(before).zip(after) {
        if a.conv_rem == b.conv_rem {
            continue;
        }
        require!(info.is_writable, ShortwinError::EpochRecordReadOnly);
        let mut data = info.try_borrow_mut_data()?;
        let end = ShareEpoch::CONV_REM_OFFSET
            .checked_add(16)
            .ok_or(ShortwinError::MathOverflow)?;
        let field = data
            .get_mut(ShareEpoch::CONV_REM_OFFSET..end)
            .ok_or(ShortwinError::EpochRecordMismatch)?;
        field.copy_from_slice(&a.conv_rem.to_le_bytes());
    }
    Ok(())
}

/// Whether the conversion that took the lender from `before` to `after` read a rebase record
/// (`num = 1`, `1/1,000^k`) of `records`, which `sync_lender` lets only the position's owner
/// do (owner decision of 2026-10-07, KI65-REV-SEC1; `RebaseNeedsOwner`, a post-condition since
/// anyone else's sync is `pool::touch_pot`, KI-66): the order of the
/// conversions through a rebase record decides who receives its rounding units (ECON §9.5).
///
/// `pool::convert` reads the record of the lender's epoch unless the lender is empty (no
/// shares, lock or lock term: the V2-12 jump reads no record). A full-wipe record leaves the
/// lender empty, so it then jumps over every later record (or parks on the live wipe record)
/// and reads no rebase record after it. Hence a conversion read a rebase record iff it moved
/// the lender's epoch, the lender was not empty, and the record of its starting epoch is a
/// rebase. Plain catch-up, the jump and the parking are not conversions through a rebase. The
/// pot-only touch's forward chain (KI-66 full) reads later rebase records for their ends and
/// ratios without converting: it moves no epoch, so it is not one either (`sync_lender` also
/// requires the records back unchanged).
pub fn reads_rebase(before: &Lender, after: &Lender, records: &[MathShareEpoch]) -> bool {
    let empty = before.shares == 0 && before.lock == 0 && before.lock_term == 0;
    after.epoch > before.epoch
        && !empty
        && records
            .iter()
            .any(|r| r.epoch == before.epoch && !r.is_full_wipe())
}

/// Fills a `LenderPosition` that `deposit` created in this instruction (`init_if_needed`
/// leaves it zeroed, so `owner == Pubkey::default()`): owner, market, bump and the math state
/// of `pool::Lender::new` (snapshots at the pool's current `acc` / `acc_r` / epoch — after the
/// prologue's accrual, as `pool_model` creates a lender after its release — `lock_pi = WAD`,
/// `lock_epoch = NO_LOCK_EPOCH`). Returns whether the position was new.
pub fn init_if_new(
    position: &mut LenderPosition,
    owner: &Pubkey,
    market: &Pubkey,
    bump: u8,
    pool: &Pool,
) -> bool {
    if position.owner != Pubkey::default() {
        return false;
    }
    position.owner = *owner;
    position.market = *market;
    position.bump = bump;
    position.set_lender(&Lender::new(pool));
    true
}

/// Whether a deposit into a pool without shares — a live full-wipe record (R4A-7) or, since
/// AIR-08, a market every lender left — also waits for `wipe_rewards_claimed` (ARCH §3.3
/// `deposit`): on the pump venues, which pay venue cashback, and on every market that has the
/// trader's pump UVA (AIR-05). The trader sweep is always required. ARCH exempts a pump venue
/// that cannot execute; telling that needs the venue's ReadSet, which `deposit` does not carry,
/// so the bit is required on every `PumpCurve`, `PumpMigrating` and `PumpSwap` market
/// (`sweep_trader` sets it whenever `S = 0` and nothing is claimable, so the deadlock argument
/// below holds in both cases).
///
/// **A market that left pump (AIR-05).** A curve market moves to `None` when its curve graduates
/// elsewhere and may then adopt a Raydium pool. `Market.pump_uva` survives both moves, and so does
/// the trader's curve-era cashback in that UVA: pump's `claim_cashback` is permissionless and does
/// not depend on the market's venue. Decided from the current venue kind, the requirement dropped
/// with the move, so a deposit (anyone's `[sol_settle (wiping), sweep_trader, deposit]`) ended the
/// wipe record before the cashback was swept, and it vested to the next epoch's lenders instead of
/// the wiped ones. The requirement therefore follows the UVA: `sweep_trader` reads the UVA on every
/// market that has one (off the pump venues the UVA alone is its token leg) and sets the bit when
/// it holds nothing claimable, so it is met exactly as on the curve — `[pump claim_cashback(user =
/// trader), sweep_trader]`, no venue CPI, no M1-FID-12 deadlock. A market listed on Raydium or
/// PumpSwap never had a pump UVA and is unchanged.
///
/// **`PumpMigrating` (M6-SEC-5).** A full wipe can settle during the migration pause (S3 is the
/// only settlement there, ARCH §5.3), with the trader's curve-era pump cashback still in its pump
/// UVA; without the requirement a deposit ended that wipe record at once and the cashback went to
/// the next epoch's lenders. `sweep_trader` treats `PumpMigrating` as a curve kind (the 5-account
/// token leg with the pump UVA), so the bit is set there exactly as on the curve — no deadlock.
///
/// **Curve (M5-SEC-1).** ARCH has `claim_venue_rewards` (M6) set the bit; on the curve kinds
/// `sweep_trader` sets it whenever the trader's pump UVA holds no claimable cashback, and pump's
/// permissionless `claim_cashback` bundled before it empties the UVA into the trader
/// (`instructions::sweep_trader` module docs). Neither needs the venue to execute nor a pump
/// CPI, so the requirement can always be met — the M1-FID-12 deadlock (a deposit waiting for
/// ever on a venue that cannot execute) does not arise on the curve.
///
/// **PumpSwap (M6; the M1-FID-12 remainder).** PumpSwap's cashback sits in pAMM's UVA WSOL ATA:
/// `sweep_trader`'s PumpSwap token leg reads it and sets the bit when it is missing or empty (and
/// the trader's pump UVA holds nothing claimable), and pAMM's permissionless `claim_cashback`
/// bundled before it moves any balance into `ATA(trader, WSOL)`, which the sweep closes and
/// credits (`instructions::sweep_trader` module docs). No PumpSwap CPI either, so the requirement
/// can always be met, whether or not the venue executes.
pub fn rewards_required(market: &Market) -> bool {
    market.pump_uva != Pubkey::default()
        || matches!(
            market.venue_kind(),
            Some(VenueKind::PumpCurve | VenueKind::PumpMigrating | VenueKind::PumpSwap)
        )
}

/// `lender_sol + protocol_fees` of `vault`: the lamports `MarketVault` must hold above its
/// rent-exempt minimum (I2).
pub fn tracked(vault: &Vault) -> Result<u64> {
    vault.lender_sol.checked_add(vault.protocol_fees).or_math()
}

/// Pays `amount` lamports out of `market_vault` to `to`, keeping the vault at or above
/// `rent_min + tracked_after`, with `tracked_after` = `lender_sol + protocol_fees` **after**
/// the payment was booked ([`tracked`]; I2). The caller must have released its `MarketVault`
/// data borrow.
pub fn pay_sol(
    market_vault: &AccountInfo,
    to: &AccountInfo,
    amount: u64,
    tracked_after: u64,
) -> Result<()> {
    lamports::move_lamports_above(market_vault, to, amount, tracked_after)
}

/// Post-condition (ARCH §3.0 step 10; I2): `market_vault` holds at least `rent_min +
/// tracked` ([`tracked`]). The caller must have released its `MarketVault` data borrow.
pub fn check_vault_floor(market_vault: &AccountInfo, tracked: u64) -> Result<()> {
    let floor = lamports::rent_min(market_vault.data_len())?
        .checked_add(tracked)
        .or_math()?;
    require_gte!(market_vault.lamports(), floor, ShortwinError::MathOverflow);
    Ok(())
}

/// `real` a claim added on top of a touch at the same `now`: the pot draw (the claim's own
/// touch adds nothing then; `pool::touch` is idempotent at a given `now`).
pub fn pot_draw(touched: &Lender, claimed: &Lender) -> u64 {
    claimed.real.saturating_sub(touched.real)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;
    use shortwin_math::num::WAD;

    #[test]
    fn a_new_position_snapshots_the_pool() {
        let mut pos = LenderPosition {
            owner: Pubkey::default(),
            market: Pubkey::default(),
            shares: 0,
            snap: 0,
            pending: 0,
            snap_r: 0,
            real: 0,
            claimed: 0,
            epoch: 0,
            lock: 0,
            lock_pi: 0,
            lock_epoch: 0,
            lock_term: 0,
            lock_until: 0,
            bump: 0,
            fwd_shares: 0,
            fwd_epoch: 0,
            fwd: 0,
            fwd_lag: 0,
            _reserved: [0; 10],
        };
        let mut p = Pool::new();
        p.acc = 7;
        p.acc_r = 5;
        p.share_epoch = 3;
        let (owner, market) = (Pubkey::new_unique(), Pubkey::new_unique());
        assert!(init_if_new(&mut pos, &owner, &market, 254, &p));
        assert_eq!((pos.snap, pos.snap_r, pos.epoch), (7, 5, 3));
        assert_eq!(
            (pos.lock_pi, pos.lock_epoch),
            (WAD, shortwin_math::pool::NO_LOCK_EPOCH)
        );
        assert_eq!((pos.owner, pos.market, pos.bump), (owner, market, 254));
        // An existing position is left alone.
        p.acc = 9;
        assert!(!init_if_new(&mut pos, &owner, &market, 254, &p));
        assert_eq!(pos.snap, 7);
    }

    /// `reads_rebase` against the math crate itself (and the pot-only touch never reads one,
    /// KI-66): a conversion reads a rebase record iff,
    /// given only the records before the first rebase record, it stops `Behind` (it needed that
    /// record), while with every record it moved the lender's epoch. Every pattern of wipe and
    /// rebase records up to three epochs behind (the last one live or not), every starting
    /// epoch, and lenders with and without shares (a dust holding, a multiple of the unit and
    /// more), a lock and a lock term.
    #[test]
    fn reads_rebase_is_the_conversion_reading_a_rebase_record() {
        use shortwin_math::pool::{convert, touch_pot, Standing};
        let rec = |epoch: u32, rebase: bool| MathShareEpoch {
            acc_end: 0,
            acc_r_end: 0,
            s_old: 1_000_000_000,
            carry: 0,
            carry_r: 0,
            conv_rem: 0,
            epoch,
            num: u8::from(rebase),
            den_log1000: u8::from(rebase),
            _pad: [0; 10],
        };
        let mut seen = [0usize; 2];
        for n in 1..=3u32 {
            for kinds in 0..(1u32 << n) {
                let recs: Vec<_> = (0..n).map(|e| rec(e, kinds >> e & 1 == 1)).collect();
                let last_wipe = recs.last().is_some_and(MathShareEpoch::is_full_wipe);
                for live in [false, true] {
                    if live && !last_wipe {
                        continue;
                    }
                    let mut p = Pool::new();
                    p.share_epoch = n;
                    p.wipe_live = u8::from(live);
                    for from in 0..n {
                        for shares in [0, 1, 999, 1_000, 1_000_123] {
                            for (lock, lock_term) in [(0, 0), (5, 0), (0, 7), (5, 7)] {
                                let mut before = Lender::new(&p);
                                before.epoch = from;
                                (before.snap, before.snap_r) = (0, 0);
                                (before.shares, before.lock, before.lock_term) =
                                    (shares, lock, lock_term);
                                let tail = &recs[from as usize..];
                                let mut after = before;
                                convert(&p, &mut after, &mut tail.to_vec()).unwrap();
                                let first = tail
                                    .iter()
                                    .position(|r| !r.is_full_wipe())
                                    .unwrap_or(tail.len());
                                let mut cut = before;
                                let st =
                                    convert(&p, &mut cut, &mut tail[..first].to_vec()).unwrap();
                                let want = st == Standing::Behind && after.epoch > before.epoch;
                                assert_eq!(
                                    reads_rebase(&before, &after, tail),
                                    want,
                                    "kinds {kinds:b} live {live} from {from} {before:?}"
                                );
                                seen[usize::from(want)] += 1;
                                // KI-66: anyone else's sync, the pot-only touch, never does
                                // (`sync_lender`'s post-condition), and stops exactly there.
                                let mut pot = before;
                                let st =
                                    touch_pot(&p, &mut Vault::new(), &mut pot, tail, 0).unwrap();
                                assert!(!reads_rebase(&before, &pot, tail));
                                if want {
                                    assert_eq!(st, Standing::Behind);
                                    assert_eq!(pot.epoch, cut.epoch);
                                    assert_eq!(pot.shares, cut.shares);
                                } else {
                                    assert_eq!(
                                        (pot.epoch, pot.shares),
                                        (after.epoch, after.shares)
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(seen[0] > 100 && seen[1] > 100, "{seen:?}");
    }

    #[test]
    fn only_pump_venues_wait_for_the_reward_claim() {
        let mut m = Market::zeroed();
        for (venue, want) in [
            (VenueKind::None, false),
            (VenueKind::PumpCurve, true),
            // M6-SEC-5: a wipe settled during the migration pause waits for the curve cashback.
            (VenueKind::PumpMigrating, true),
            (VenueKind::PumpSwap, true),
            (VenueKind::RaydiumV4, false),
            (VenueKind::RaydiumCpmm, false),
        ] {
            m.venue = venue as u8;
            assert_eq!(rewards_required(&m), want, "{venue:?}");
        }
    }
}
