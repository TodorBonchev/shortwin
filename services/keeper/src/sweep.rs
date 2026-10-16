//! The trader side's value and the venue cashback (ARCHITECTURE §3.3, §3.9 `sweep_trader`, §7.1
//! duty 6 "right after a full wipe" and duty 13; ECON §9.5; R4A-7; M5-SEC-1 and its M6 remainder):
//! what the keeper sends so that lamports and tokens left on a market's trader side, and the
//! cashback the venues accrue to the market's `trader` PDA, reach the lenders.
//!
//! **The post-wipe bundle** (duty 6, [`wipe_pending`], [`post_wipe_ixs`]). While the pool has no
//! shares (`S = 0`: after a full wipe, with a live wipe record, or — since AIR-08 — after the last
//! lender's exit, which clears both bits) a deposit waits for `wipe_trader_swept` and, on the pump
//! venues (`PumpCurve`, `PumpMigrating`, `PumpSwap`) and on every market that has the trader's pump
//! UVA (a curve market that left pump, AIR-05; `engine::lender::rewards_required`),
//! `wipe_rewards_claimed` (R4A-7) — until then the market takes no deposit and hence no open. Off
//! the pump venues the sweep's token leg is that UVA alone ([`sweep_trader_ix`]). Both are set by
//! `sweep_trader` (the program's `claim_venue_rewards` is not built: M5-SEC-1 and M6's
//! UVA-reading pattern, ARCH §3.9): the trader bit always, the rewards bit when the trader's venue
//! cashback is not claimable — the pump UVA's lamports above its rent-exempt minimum, and on
//! PumpSwap the WSOL of `ATA(pAMM UVA(trader), WSOL)`. With cashback outstanding the keeper
//! prepends the venues' own permissionless claims, in one transaction:
//!
//! | venue | transaction |
//! |---|---|
//! | nothing claimable | `sweep_trader` |
//! | pump UVA above rent | pump `claim_cashback(user = trader)` (lamports to the trader), `sweep_trader` |
//! | pAMM UVA's WSOL ATA non-empty (PumpSwap) | `create_idempotent(ATA(trader, WSOL))`, pAMM `claim_cashback(user = trader, user_wsol = ATA(trader, WSOL))`, `sweep_trader` (it closes that ATA into the trader) |
//! | the same, the trader's pAMM UVA missing (M6-SEC-2) | PumpSwap `init_user_volume_accumulator(payer = keeper, user = trader)` first, then the row above |
//!
//! No claim needs the trader's signature (T-CASHBACK, verified on both dumped ELFs), and neither
//! needs the venue to execute (`disable_flags` does not gate `claim_cashback`). The ATA's rent the
//! keeper pays reaches the wiped epoch's lenders with the cashback (the sweep credits everything
//! the trader holds).
//!
//! **A funded cashback ATA without its UVA (M6-SEC-2).** Anyone can create `ATA(pAMM UVA(trader),
//! WSOL)` — an ATA of any address — and put WSOL in it, whether or not the trader's pAMM UVA
//! exists; `sweep_trader` then leaves the rewards bit clear, and pAMM's `claim_cashback` fails 3012
//! (`AccountNotInitialized`, on `user_volume_accumulator`) while the UVA does not exist (R-VEN Update
//! H7). The keeper's `prepare_pamm_trader` duty creates the UVA of every PumpSwap market whose pool
//! and `GlobalConfig` parse, but not otherwise (a pAMM layout change, A63), and a client building
//! this transaction has no such duty: the claims therefore create the UVA themselves when it is
//! missing (any payer, `user` unsigned; R-VEN Update H6), so the transaction is complete on its own.
//! The unlock still depends on pAMM's `claim_cashback` existing with this interface (a venue-interface
//! item: a WSOL donation turns its removal into a lasting deposit lock; re-verify on every pAMM
//! upgrade).
//!
//! **Duty 13** ([`sweep_due`], [`cashback_claim_due`]): `sweep_trader` when the trader side holds
//! anything — `ATA(trader, WSOL)` exists (anyone can create and fund it; no leg uses it), the
//! `trader` or `trader_wsol` PDA holds lamports (a donation), or, on a pump venue, the trader's
//! base ATA holds tokens — checked every `sweep.check_interval_s`; and the venue cashback claim
//! with a sweep, on a cashback coin whose claimable cashback reaches `sweep.cashback_min_lamports`,
//! at most every `sweep.cashback_interval_s` (ARCH: weekly).

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::{InstructionData, ToAccountMetas};
use shortwin::constants::{
    PUMP_AMM_EVENT_AUTHORITY, PUMP_AMM_PROGRAM_ID, PUMP_EVENT_AUTHORITY, PUMP_PROGRAM_ID,
    TOKEN_PROGRAM_ID, WSOL_MINT,
};
use shortwin::state::{
    MARKET_FLAG_WIPE_LIVE, MARKET_FLAG_WIPE_REWARDS_CLAIMED, MARKET_FLAG_WIPE_TRADER_SWEPT, Market,
    VenueKind, pump_swap_cache,
};
use shortwin::venues::{pump_amm, pump_curve};
use shortwin_services_common::AccountData;

use crate::graduation::{create_ata_idempotent_ix, pamm_init_uva_ix};
use crate::pda;

/// The Rent sysvar (the pump UVA's rent-exempt minimum, as the program's `sweep_trader` reads it).
pub const SYSVAR_RENT: Pubkey = crate::graduation::SYSVAR_RENT;

/// `claim_cashback`'s discriminator (the same Anchor name on pump and PumpSwap; ARCH [V11]).
pub const IX_CLAIM_CASHBACK: [u8; 8] = pump_amm::IX_CLAIM_CASHBACK;

/// The rent-exempt minimum of a `len`-byte account under the Rent sysvar `data` (`lamports_per_byte
/// (_year)` u64, `exemption_threshold` f64, `burn_percent` u8; `Rent::minimum_balance`), `None`
/// if it does not parse.
pub fn rent_minimum(data: &[u8], len: usize) -> Option<u64> {
    let per_byte = u64::from_le_bytes(data.get(0..8)?.try_into().ok()?);
    let threshold = f64::from_le_bytes(data.get(8..16)?.try_into().ok()?);
    let bytes = u64::try_from(len).ok()?.checked_add(128)?;
    let base = bytes.checked_mul(per_byte)?;
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let min = (base as f64 * threshold) as u64;
    Some(min)
}

/// The pool has no shares and a deposit still waits for a sweep (R4A-7; AIR-08): the trader bit,
/// or — on a pump venue, `PumpMigrating` included (M6-SEC-5), and on any market with the trader's
/// pump UVA (AIR-05) — the rewards bit is clear (the program's `deposit` check,
/// `engine::lender::rewards_required`, `WipeSweepPending`). Since AIR-08 this holds after the last
/// lender's exit as well as after a full wipe: `withdraw` clears both bits when `S` reaches 0, so
/// the gate is `S = 0`, not the wipe record (a new market starts with both bits set and is not
/// pending).
pub fn wipe_pending(market: &Market) -> bool {
    let rewards = shortwin::engine::lender::rewards_required(market);
    market.total_shares.get() == 0
        && (!market.has_flag(MARKET_FLAG_WIPE_TRADER_SWEPT)
            || (rewards && !market.has_flag(MARKET_FLAG_WIPE_REWARDS_CLAIMED)))
}

/// A full-wipe record is live (the sweep then names the latest `ShareEpoch` record).
pub fn wipe_live(market: &Market) -> bool {
    market.total_shares.get() == 0 && market.has_flag(MARKET_FLAG_WIPE_LIVE)
}

/// A pump venue (`sweep_trader` carries the token leg).
fn pump_kind(market: &Market) -> bool {
    matches!(
        market.venue_kind(),
        Some(VenueKind::PumpCurve | VenueKind::PumpMigrating | VenueKind::PumpSwap)
    )
}

/// `ATA(pAMM UVA(trader), WSOL)`: where PumpSwap accrues the trader's cashback (the PumpSwap
/// cache, else derived).
pub fn pamm_cashback_ata(market: &Market) -> Pubkey {
    match pump_amm::cached(market, pump_swap_cache::PAMM_UVA_QUOTE_ATA) {
        k if k == Pubkey::default() => {
            pump_curve::wsol_ata(&pump_amm::user_volume_accumulator(&market.trader))
        }
        k => k,
    }
}

/// `sweep_trader()` of `market_key` (ARCH §3.9 as built): the fixed accounts, then on a pump venue
/// its token leg `[trader_base_ata W, pool_vault W, mint, token program, the trader's pump UVA]`
/// (+ on PumpSwap `ATA(pAMM UVA(trader), WSOL)`, M6) — off the pump venues the trader's pump UVA
/// alone when the market has one (AIR-05) —, then the latest `ShareEpoch` record (W) while a
/// full-wipe record is live.
pub fn sweep_trader_ix(market_key: &Pubkey, market: &Market) -> Instruction {
    let mut metas = shortwin::accounts::SweepTrader {
        config: pda::config(),
        market: *market_key,
        market_vault: market.market_vault,
        trader: market.trader,
        trader_wsol: market.trader_wsol,
        trader_wsol_ata: pump_curve::trader_wsol_ata(market),
        token_program: TOKEN_PROGRAM_ID,
        system_program: anchor_lang::system_program::ID,
        event_authority: pda::event_authority(),
        program: shortwin::ID,
    }
    .to_account_metas(None);
    if pump_kind(market) {
        metas.extend([
            AccountMeta::new(market.trader_base_ata, false),
            AccountMeta::new(market.pool_vault, false),
            AccountMeta::new_readonly(market.mint, false),
            AccountMeta::new_readonly(market.token_program, false),
            AccountMeta::new_readonly(market.pump_uva, false),
        ]);
        if market.venue_kind() == Some(VenueKind::PumpSwap) {
            metas.push(AccountMeta::new_readonly(pamm_cashback_ata(market), false));
        }
    } else if market.pump_uva != Pubkey::default() {
        // AIR-05: a market that left pump keeps its trader's pump UVA (curve-era cashback).
        metas.push(AccountMeta::new_readonly(market.pump_uva, false));
    }
    if wipe_live(market) {
        metas.push(AccountMeta::new(
            pda::share_epoch(market_key, market.share_epoch.saturating_sub(1)),
            false,
        ));
    }
    Instruction {
        program_id: shortwin::ID,
        accounts: metas,
        data: shortwin::instruction::SweepTrader {}.data(),
    }
}

/// pump `claim_cashback` for `user` (`[user W, user_volume_accumulator W, system, event_authority,
/// program]`; `user` does not sign — T-CASHBACK, pump part): the UVA's lamports above rent to
/// `user`.
pub fn pump_claim_cashback_ix(user: &Pubkey) -> Instruction {
    Instruction {
        program_id: PUMP_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*user, false),
            AccountMeta::new(pump_curve::user_volume_accumulator(user), false),
            AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
            AccountMeta::new_readonly(PUMP_EVENT_AUTHORITY, false),
            AccountMeta::new_readonly(PUMP_PROGRAM_ID, false),
        ],
        data: IX_CLAIM_CASHBACK.to_vec(),
    }
}

/// PumpSwap `claim_cashback` for `user` into its WSOL account `user_wsol` (9 accounts: `user` W,
/// the UVA W, WSOL, Tokenkeg, `ATA(UVA, WSOL)` W, `user_wsol` W, System, event authority, program;
/// `user` does not sign — T-CASHBACK, pAMM part, R-VEN Update H7): the ATA's whole WSOL balance to
/// `user_wsol`.
pub fn pamm_claim_cashback_ix(user: &Pubkey, uva: &Pubkey, user_wsol: &Pubkey) -> Instruction {
    Instruction {
        program_id: PUMP_AMM_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*user, false),
            AccountMeta::new(*uva, false),
            AccountMeta::new_readonly(WSOL_MINT, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new(pump_curve::wsol_ata(uva), false),
            AccountMeta::new(*user_wsol, false),
            AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
            AccountMeta::new_readonly(PUMP_AMM_EVENT_AUTHORITY, false),
            AccountMeta::new_readonly(PUMP_AMM_PROGRAM_ID, false),
        ],
        data: IX_CLAIM_CASHBACK.to_vec(),
    }
}

/// The number of [`fact_keys`].
pub const FACT_KEYS_LEN: usize = 8;

/// The accounts the keeper reads for a market's sweep and claim decisions, in order
/// ([`SweepFacts::of`]): `ATA(trader, WSOL)`, `trader`, `trader_wsol`, the trader's base ATA, its
/// pump UVA, `ATA(pAMM UVA(trader), WSOL)`, the Rent sysvar, the trader's pAMM UVA.
pub fn fact_keys(market: &Market) -> [Pubkey; FACT_KEYS_LEN] {
    [
        pump_curve::trader_wsol_ata(market),
        market.trader,
        market.trader_wsol,
        market.trader_base_ata,
        market.pump_uva,
        pamm_cashback_ata(market),
        SYSVAR_RENT,
        crate::venue::uva_of(market),
    ]
}

/// What the trader side of a market holds (from [`fact_keys`]' accounts).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SweepFacts {
    /// `ATA(trader, WSOL)` is an initialised SPL Token account (its lamports, rent included).
    pub trader_ata: Option<u64>,
    /// Lamports on the `trader` and `trader_wsol` PDAs (0 between instructions unless donated).
    pub pda_lamports: u64,
    /// Tokens on the trader's base ATA (a pump venue's; 0 between instructions unless donated).
    pub base_tokens: u64,
    /// pump's claimable cashback: the trader's pump UVA's lamports above its rent-exempt minimum.
    pub pump_claimable: u64,
    /// PumpSwap's claimable cashback: the WSOL in `ATA(pAMM UVA(trader), WSOL)` (a PumpSwap
    /// market's only).
    pub pamm_claimable: u64,
    /// The trader's pAMM UVA exists (owned by PumpSwap, with data): pAMM's `claim_cashback` fails
    /// 3012 without it (M6-SEC-2).
    pub pamm_uva: bool,
}

fn token_amount(acc: &AccountData) -> Option<u64> {
    shortwin_venue_state::token_account::read(&acc.owner.to_bytes(), &acc.data)
        .ok()
        .map(|t| t.amount)
}

impl SweepFacts {
    /// The facts of `market` from [`fact_keys`]' accounts.
    pub fn of(market: &Market, accs: &[Option<AccountData>]) -> Self {
        let acc = |i: usize| accs.get(i).and_then(Option::as_ref);
        let trader_ata = acc(0)
            .filter(|a| a.owner == TOKEN_PROGRAM_ID && !a.data.is_empty())
            .map(|a| a.lamports);
        let pda_lamports = acc(1)
            .map_or(0, |a| a.lamports)
            .saturating_add(acc(2).map_or(0, |a| a.lamports));
        let base_tokens = if pump_kind(market) {
            acc(3).and_then(token_amount).unwrap_or(0)
        } else {
            0
        };
        let pump_claimable = match (acc(4), acc(6)) {
            (Some(uva), Some(rent)) if uva.owner == PUMP_PROGRAM_ID => {
                rent_minimum(&rent.data, uva.data.len())
                    .map_or(0, |min| uva.lamports.saturating_sub(min))
            }
            _ => 0,
        };
        let pamm_claimable = if market.venue_kind() == Some(VenueKind::PumpSwap) {
            acc(5)
                .filter(|a| a.owner == TOKEN_PROGRAM_ID)
                .and_then(token_amount)
                .unwrap_or(0)
        } else {
            0
        };
        let pamm_uva = acc(7).is_some_and(|a| a.owner == PUMP_AMM_PROGRAM_ID && !a.data.is_empty());
        Self {
            trader_ata,
            pda_lamports,
            base_tokens,
            pump_claimable,
            pamm_claimable,
            pamm_uva,
        }
    }

    /// Duty 13: the trader side holds something a `sweep_trader` moves to the lenders.
    pub fn sweep_due(&self) -> bool {
        self.trader_ata.is_some() || self.pda_lamports > 0 || self.base_tokens > 0
    }

    /// The venue cashback claimable now (pump and PumpSwap together).
    pub fn claimable(&self) -> u64 {
        self.pump_claimable.saturating_add(self.pamm_claimable)
    }
}

/// The claims to prepend to a `sweep_trader` of `market` (module docs): pump's when the pump UVA
/// holds cashback above rent, PumpSwap's (with `ATA(trader, WSOL)` created idempotently by
/// `payer`) when the pAMM UVA's WSOL ATA holds any — preceded by the trader's pAMM UVA, created by
/// `payer`, when it does not exist (M6-SEC-2: the claim fails 3012 without it).
pub fn claim_ixs(payer: &Pubkey, market: &Market, facts: &SweepFacts) -> Vec<Instruction> {
    let mut v = Vec::new();
    if facts.pump_claimable > 0 {
        v.push(pump_claim_cashback_ix(&market.trader));
    }
    if facts.pamm_claimable > 0 {
        let uva = crate::venue::uva_of(market);
        if !facts.pamm_uva {
            v.push(pamm_init_uva_ix(payer, &market.trader));
        }
        v.push(create_ata_idempotent_ix(
            payer,
            &market.trader,
            &WSOL_MINT,
            &TOKEN_PROGRAM_ID,
        ));
        v.push(pamm_claim_cashback_ix(
            &market.trader,
            &uva,
            &pump_curve::trader_wsol_ata(market),
        ));
    }
    v
}

/// The post-wipe transaction of `market_key` (module docs): the claims, then `sweep_trader`.
pub fn post_wipe_ixs(
    payer: &Pubkey,
    market_key: &Pubkey,
    market: &Market,
    facts: &SweepFacts,
) -> Vec<Instruction> {
    let mut v = claim_ixs(payer, market, facts);
    v.push(sweep_trader_ix(market_key, market));
    v
}

/// One sweep or claim the keeper sent (duties 6 and 13).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SweepKind {
    /// While `S = 0` and a deposit waits for the sweep — right after a full wipe (duty 6) or,
    /// since AIR-08, after the last lender's exit: the claims needed and `sweep_trader`.
    PostWipe,
    /// The trader side held something (duty 13).
    Sweep,
    /// The venue cashback claim with a sweep (duty 13).
    Cashback,
}

/// One sweep attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SweepRecord {
    pub market: Pubkey,
    pub kind: SweepKind,
    /// What the keeper read before it.
    pub facts: SweepFacts,
    /// The instructions sent (claims and ATA creations included).
    pub instructions: usize,
    pub result: crate::keeper::ExitResult,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rent(per_byte: u64, threshold: f64) -> Vec<u8> {
        let mut d = per_byte.to_le_bytes().to_vec();
        d.extend_from_slice(&threshold.to_le_bytes());
        d.push(50);
        d
    }

    /// `Rent::minimum_balance`: (128 + len)·per_byte·threshold.
    #[test]
    fn rent_minimum_is_the_sysvars_formula() {
        assert_eq!(rent_minimum(&rent(3_480, 2.0), 0), Some(890_880));
        assert_eq!(rent_minimum(&rent(2_540, 2.0), 0), Some(650_240));
        assert_eq!(rent_minimum(&rent(5_080, 1.0), 137), Some(1_346_200));
        assert_eq!(rent_minimum(&[0; 4], 0), None);
    }

    fn market(kind: VenueKind) -> Market {
        let mut m: Market = bytemuck::Zeroable::zeroed();
        m.venue = kind as u8;
        m.trader = Pubkey::new_unique();
        m.trader_wsol = Pubkey::new_unique();
        m.market_vault = Pubkey::new_unique();
        m.trader_base_ata = Pubkey::new_unique();
        m.pool_vault = Pubkey::new_unique();
        m.mint = Pubkey::new_unique();
        m.token_program = Pubkey::new_unique();
        m.pump_uva = Pubkey::new_unique();
        m
    }

    /// The program's `sweep_trader` account list (ARCH §3.9 as built): 10 fixed, the token leg on
    /// the pump venues (5, PumpSwap 6) — off them the trader's pump UVA alone where the market has
    /// one (AIR-05) —, the wipe record while a full wipe is live.
    #[test]
    fn sweep_trader_names_the_token_leg_and_the_wipe_record() {
        let key = Pubkey::new_unique();
        let mut m = market(VenueKind::RaydiumCpmm);
        m.pump_uva = Pubkey::default();
        assert_eq!(
            sweep_trader_ix(&key, &m).accounts.len(),
            10,
            "listed on Raydium"
        );
        // AIR-05: a curve market that left pump (None, then Raydium) keeps its pump UVA.
        for kind in [VenueKind::None, VenueKind::RaydiumCpmm] {
            let mut m = market(kind);
            let ix = sweep_trader_ix(&key, &m);
            assert_eq!(ix.accounts.len(), 11, "{kind:?}");
            assert_eq!(ix.accounts[10].pubkey, m.pump_uva);
            assert!(!ix.accounts[10].is_writable);
            m.flags |= MARKET_FLAG_WIPE_LIVE;
            m.share_epoch = 1;
            let ix = sweep_trader_ix(&key, &m);
            assert_eq!(ix.accounts.len(), 12);
            assert_eq!(ix.accounts[11].pubkey, pda::share_epoch(&key, 0));
        }
        let m = market(VenueKind::PumpCurve);
        let ix = sweep_trader_ix(&key, &m);
        assert_eq!(ix.accounts.len(), 15);
        assert_eq!(ix.accounts[10].pubkey, m.trader_base_ata);
        assert!(ix.accounts[10].is_writable && ix.accounts[11].is_writable);
        assert_eq!(ix.accounts[14].pubkey, m.pump_uva);
        let mut m = market(VenueKind::PumpSwap);
        let ix = sweep_trader_ix(&key, &m);
        assert_eq!(ix.accounts.len(), 16);
        assert_eq!(ix.accounts[15].pubkey, pamm_cashback_ata(&m));
        m.flags |= MARKET_FLAG_WIPE_LIVE;
        m.share_epoch = 3;
        let ix = sweep_trader_ix(&key, &m);
        assert_eq!(ix.accounts.len(), 17);
        assert_eq!(ix.accounts[16].pubkey, pda::share_epoch(&key, 2));
        assert!(ix.accounts[16].is_writable);
        assert!(ix.accounts.iter().all(|a| !a.is_signer));
    }

    /// R4A-7, AIR-08: while `S = 0` — after a full wipe or after the last lender's exit (which
    /// clears both bits) — pending until the trader bit and, on the pump venues and on any market
    /// with the trader's pump UVA (AIR-05), the rewards bit are set; never while lenders hold
    /// shares.
    #[test]
    fn wipe_pending_follows_the_deposit_check() {
        for (kind, uva, rewards) in [
            (VenueKind::PumpSwap, true, true),
            (VenueKind::PumpSwap, false, true),
            (VenueKind::PumpCurve, true, true),
            // M6-SEC-5: a full wipe settled during the migration pause waits for the curve-era
            // cashback as well.
            (VenueKind::PumpMigrating, true, true),
            // AIR-05: so does one on a curve market that left pump.
            (VenueKind::RaydiumV4, true, true),
            (VenueKind::None, true, true),
            // Markets that never had a pump UVA.
            (VenueKind::RaydiumV4, false, false),
            (VenueKind::None, false, false),
        ] {
            for wipe in [true, false] {
                let mut m = market(kind);
                if !uva {
                    m.pump_uva = Pubkey::default();
                }
                m.total_shares = shortwin::state::PodU128::new(1_000);
                assert!(!wipe_pending(&m), "{kind:?}: lenders hold shares");
                m.total_shares = shortwin::state::PodU128::new(0);
                if wipe {
                    m.flags |= MARKET_FLAG_WIPE_LIVE;
                }
                assert!(wipe_pending(&m), "{kind:?}, wipe {wipe}: S = 0, bits clear");
                m.flags |= MARKET_FLAG_WIPE_TRADER_SWEPT;
                assert_eq!(
                    wipe_pending(&m),
                    rewards,
                    "{kind:?}, uva {uva}, wipe {wipe}"
                );
                m.flags |= MARKET_FLAG_WIPE_REWARDS_CLAIMED;
                assert!(!wipe_pending(&m), "{kind:?}, wipe {wipe}");
            }
        }
    }

    /// No claim needs a signature; the PumpSwap claim goes to `ATA(trader, WSOL)`, created first.
    #[test]
    fn claims_are_unsigned_and_ordered() {
        let m = market(VenueKind::PumpSwap);
        let payer = Pubkey::new_unique();
        let facts = SweepFacts {
            pump_claimable: 1,
            pamm_claimable: 2,
            pamm_uva: true,
            ..SweepFacts::default()
        };
        let v = post_wipe_ixs(&payer, &Pubkey::new_unique(), &m, &facts);
        assert_eq!(v.len(), 4);
        assert_eq!(v[0].program_id, PUMP_PROGRAM_ID);
        assert_eq!(v[1].accounts[2].pubkey, m.trader);
        assert_eq!(v[2].program_id, PUMP_AMM_PROGRAM_ID);
        assert_eq!(v[2].accounts[5].pubkey, pump_curve::trader_wsol_ata(&m));
        assert_eq!(v[3].program_id, shortwin::ID);
        for ix in &v[..3] {
            assert!(
                ix.accounts
                    .iter()
                    .all(|a| !a.is_signer || a.pubkey == payer)
            );
        }
        let h = |n: &str| {
            let d = solana_sha256_hasher::hash(format!("global:{n}").as_bytes());
            <[u8; 8]>::try_from(&d.to_bytes()[..8]).unwrap()
        };
        assert_eq!(IX_CLAIM_CASHBACK, h("claim_cashback"));
    }

    /// M6-SEC-2: PumpSwap cashback outstanding while the trader's pAMM UVA does not exist (anyone
    /// can create and fund `ATA(UVA, WSOL)` for a UVA that was never created): the claims create
    /// the UVA first — `init_user_volume_accumulator(payer, user = trader)`, only the payer signs —
    /// so pAMM's `claim_cashback` does not fail 3012; with the UVA present nothing is added, and
    /// without PumpSwap cashback no UVA is created.
    #[test]
    fn a_missing_pamm_uva_is_created_before_the_claim() {
        let m = market(VenueKind::PumpSwap);
        let payer = Pubkey::new_unique();
        let facts = SweepFacts {
            pamm_claimable: 5,
            ..SweepFacts::default()
        };
        assert!(!facts.pamm_uva);
        let v = claim_ixs(&payer, &m, &facts);
        assert_eq!(v.len(), 3, "UVA, ATA(trader, WSOL), claim");
        assert_eq!(v[0].program_id, PUMP_AMM_PROGRAM_ID);
        assert_eq!(v[0].data, pump_amm::IX_INIT_USER_VOLUME_ACCUMULATOR);
        assert_eq!(v[0].accounts[0].pubkey, payer);
        assert!(v[0].accounts[0].is_signer);
        assert_eq!(v[0].accounts[1].pubkey, m.trader);
        assert_eq!(v[0].accounts[2].pubkey, crate::venue::uva_of(&m));
        assert!(v[0].accounts[1..].iter().all(|a| !a.is_signer));
        assert_eq!(v[2].data, IX_CLAIM_CASHBACK);
        assert_eq!(v[2].accounts[1].pubkey, crate::venue::uva_of(&m));
        let with_uva = SweepFacts {
            pamm_uva: true,
            ..facts
        };
        assert_eq!(claim_ixs(&payer, &m, &with_uva).len(), 2);
        let nothing = SweepFacts::default();
        assert!(claim_ixs(&payer, &m, &nothing).is_empty());
    }

    /// `SweepFacts::of` reads the pAMM UVA (the 8th fact account): present only when PumpSwap owns
    /// it with data.
    #[test]
    fn the_facts_read_the_pamm_uva() {
        let m = market(VenueKind::PumpSwap);
        assert_eq!(fact_keys(&m)[7], crate::venue::uva_of(&m));
        let acc = |owner: Pubkey, len: usize| AccountData {
            lamports: 1_346_200,
            owner,
            data: vec![0; len],
            executable: false,
        };
        let mut accs: Vec<Option<AccountData>> = vec![None; 8];
        assert!(!SweepFacts::of(&m, &accs).pamm_uva);
        accs[7] = Some(acc(anchor_lang::system_program::ID, 0));
        assert!(!SweepFacts::of(&m, &accs).pamm_uva);
        accs[7] = Some(acc(PUMP_AMM_PROGRAM_ID, 137));
        assert!(SweepFacts::of(&m, &accs).pamm_uva);
    }
}
