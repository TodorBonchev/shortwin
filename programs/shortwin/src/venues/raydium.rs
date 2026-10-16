//! What the two Raydium adapters share ([`super::raydium_v4`], [`super::raydium_cpmm`]; ARCH
//! §2.4, §2.4.1 Raydium row, §2.6, §3.2, §3.12; R-RAY §4, §6.2, §9.3).
//!
//! **Legs** (ARCH §2.4.1, the Raydium row; the mock venue's legs are the template, M2):
//!
//! - [`delegate_sell`] (open, exact in): `approve_checked(pool_vault → trader, Q)` signed by the
//!   market — the delegate is limited to exactly `Q`; the ephemeral `trader_wsol` created
//!   grief-proof (allocate / assign, never `create_account`; the borrower pays its rent for the
//!   length of the leg); the venue's exact-in swap spends `Q` **from the lending vault itself**
//!   with `trader` signing as its delegate, into `trader_wsol`; then `S = trader_wsol.amount`,
//!   the vault's delta must be exactly `−Q` (`VenueResultMismatch`) and its delegate gone
//!   (`DelegateNotCleared`: p-token and Token-2022 clear a delegate whose allowance reaches 0,
//!   ARCH `[V12]`, so after an exact-`Q` spend there is nothing left to revoke; a venue that
//!   spent less would leave both the delta and the delegate wrong, which reverts);
//!   `trader_wsol` is closed into the position.
//! - [`direct_buy`] (exits, exact out): `trader_wsol` funded with `rent(165) + max_in` straight
//!   from the position (direct debit, no CPI); the venue's exact-out swap pays from
//!   `trader_wsol` and **delivers straight into the lending vault** (neither venue checks the
//!   destination's owner, R-RAY §3.1–§3.2); the vault must receive at least `Q`
//!   (`VenueResultMismatch`; a surplus is a donation, A62, V2-10); `trader_wsol` is closed back
//!   into the position; `B = position lamports before − after ≤ max_in` (`ert = 0` on
//!   Raydium: no venue-side rent, R-RAY §4.1).
//!
//! The caller normalised the trader PDAs first (ARCH §2.4 rule 2) and checks the leg's
//! post-conditions after it (rule 5, I7). The swap itself is the adapter's
//! ([`invoke_as_trader`]: the CPI metas mark only `trader` as a signer, rule 4).
//!
//! **Listing and pool selection** ([`Adoption`], [`check_eligible`], [`position_floor_ok`]):
//! each adapter verifies its candidate pool (ARCH §2.6 "Verification of a candidate") into an
//! [`Adoption`]; [`check_eligible`] is `eligible()` with one error per failing clause,
//! [`position_floor_ok`] the stricter floor of a collection window's candidate (M4V-1);
//! [`Adoption::apply`]
//! writes a new market's pool cache, SOL-side flag, locked depth and pool age (O13), and
//! [`Adoption::write_pool`] the pool alone for a re-adoption (`crate::engine::pool_select`, M4).

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke_signed;
use anchor_spl::token_interface::{self, ApproveChecked};
use shortwin_math::fees;
use shortwin_math::pool_select::{self, EligibilityParams, PoolFacts};
use shortwin_venue_state::mint;
use shortwin_venue_state::token_account::TokenAccount;

use super::{BuyOut, Leg, VenueState};
use crate::constants::TOKEN_PROGRAM_ID;
use crate::engine::trader::{token_state, Funding};
use crate::errors::{CheckedExt, ShortwinError};
use crate::state::{raydium_cache, Market, Params, VenueKind, MARKET_FLAG_SOL_IS_TOKEN0_OR_COIN};

/// The market's pool-cache entry `i` (`raydium_cache`), the default key if out of range.
pub fn cached(market: &Market, i: usize) -> Pubkey {
    market.pool_cache.get(i).copied().unwrap_or_default()
}

/// WSOL is the AMM v4 coin / the CPMM token 0 (`Market.flags` bit 3).
pub fn sol_first(market: &Market) -> bool {
    market.has_flag(MARKET_FLAG_SOL_IS_TOKEN0_OR_COIN)
}

/// The account at slot `i` of a key-checked list (`BadRemainingAccounts` if missing).
pub fn at<'a, 'info>(
    accounts: &'a [AccountInfo<'info>],
    i: usize,
) -> Result<&'a AccountInfo<'info>> {
    accounts
        .get(i)
        .ok_or_else(|| error!(ShortwinError::BadRemainingAccounts))
}

/// Key-checks `accounts` against `want`, slot by slot, with exactly `want.len()` accounts
/// (`BadRemainingAccounts` for a wrong count, `VenueAccountMismatch` for a wrong key).
pub fn check_keys(accounts: &[AccountInfo], want: &[Pubkey]) -> Result<()> {
    require!(
        accounts.len() == want.len(),
        ShortwinError::BadRemainingAccounts
    );
    for (info, key) in accounts.iter().zip(want.iter()) {
        require_keys_eq!(info.key(), *key, ShortwinError::VenueAccountMismatch);
    }
    Ok(())
}

/// A token account owned by `program`, parsed; `None` if it is not one.
pub fn token_of(info: &AccountInfo, program: &Pubkey) -> Option<TokenAccount> {
    if info.owner != program {
        return None;
    }
    token_state(info).ok()
}

/// The circulating supply of an SPL Token LP mint (both Raydium programs mint LP under
/// Tokenkeg; `supply` @36); `None` if it is not one.
pub fn lp_supply(info: &AccountInfo) -> Option<u64> {
    if *info.owner != TOKEN_PROGRAM_ID {
        return None;
    }
    let data = info.try_borrow_data().ok()?;
    mint::read_base(&info.owner.to_bytes(), &data)
        .ok()
        .map(|m| m.supply)
}

/// A verified candidate pool (ARCH §2.6), ready to be adopted by `create_market`.
#[derive(Clone, Debug)]
pub struct Adoption {
    pub kind: VenueKind,
    pub pool: Pubkey,
    /// The WSOL-side and token-side vaults.
    pub vault_sol: Pubkey,
    pub vault_tok: Pubkey,
    /// CPMM only (the default key on AMM v4).
    pub amm_config: Pubkey,
    /// CPMM only (the default key on AMM v4).
    pub observation: Pubkey,
    pub lp_mint: Pubkey,
    /// The token side's program (`market.token_program`).
    pub tok_program: Pubkey,
    /// WSOL is the AMM v4 coin / the CPMM token 0.
    pub sol_first: bool,
    /// The pool's live state (executable, eligible).
    pub state: VenueState,
    /// `graduated_at` (ARCH §3.2, §4.7; SPEC §4 O13): the listing time on AMM v4; on CPMM
    /// `min(now, open_time)` when `open_time ≥ T_CLAMP`, else the listing time.
    pub age_start: i64,
}

impl Adoption {
    /// The candidate's ReadSet keys in its adapter's order (ARCH §3.12), padded to the six
    /// slots of the fast-path window (`Market.fp_keys`, ARCH §2.3.4): AMM v4 `[pool, coin vault,
    /// pc vault, LP mint, default, default]`; CPMM `[pool, amm_config, token 0 vault, token 1
    /// vault, LP mint, observation]` (the observation is not in the ReadSet: it is the pool's
    /// own `observation_key`, kept so `execute_pool_switch` adopts exactly what was proposed).
    pub fn window_keys(&self) -> [Pubkey; 6] {
        let (first, second) = if self.sol_first {
            (self.vault_sol, self.vault_tok)
        } else {
            (self.vault_tok, self.vault_sol)
        };
        match self.kind {
            VenueKind::RaydiumCpmm => [
                self.pool,
                self.amm_config,
                first,
                second,
                self.lp_mint,
                self.observation,
            ],
            _ => [
                self.pool,
                first,
                second,
                self.lp_mint,
                Pubkey::default(),
                Pubkey::default(),
            ],
        }
    }

    /// Writes the pool itself into `market` (ARCH §2.6 rule 4, §3.2 "Effects"): the venue, the
    /// whole pool cache (`raydium_cache`; every other slot cleared, so nothing of a previous
    /// venue's union stays), the SOL-side flag and the locked depth `pool_d`. The age, the
    /// listing window, the price state and the chains are the caller's.
    pub fn write_pool(&self, market: &mut Market) -> Result<()> {
        market.pool_cache = [Pubkey::default(); 10];
        let entries = [
            (raydium_cache::RAY_POOL, self.pool),
            (raydium_cache::RAY_VAULT_SOL, self.vault_sol),
            (raydium_cache::RAY_VAULT_TOK, self.vault_tok),
            (raydium_cache::RAY_AMM_CONFIG, self.amm_config),
            (raydium_cache::RAY_OBSERVATION, self.observation),
            (raydium_cache::RAY_LP_MINT, self.lp_mint),
            (raydium_cache::RAY_TOK_PROGRAM, self.tok_program),
        ];
        for (i, key) in entries {
            *market
                .pool_cache
                .get_mut(i)
                .ok_or(ShortwinError::MathOverflow)? = key;
        }
        market.set_flag(MARKET_FLAG_SOL_IS_TOKEN0_OR_COIN, self.sol_first);
        market.pool_d.set(self.state.locked_depth);
        market.venue = self.kind as u8;
        Ok(())
    }

    /// Writes the adopted pool into a new market (ARCH §3.2 "Effects", §2.6 rule 1): the pool
    /// ([`Adoption::write_pool`]), `graduated_at` (O13) and `pool_seen_at = now` (opens wait
    /// `pool_list_window_s`).
    pub fn apply(&self, market: &mut Market, now: i64) -> Result<()> {
        self.write_pool(market)?;
        market.graduated_at = self.age_start;
        market.pool_seen_at = now;
        Ok(())
    }
}

/// `eligible(P)` of ARCH §2.6 as amended by the M4 security review, on a verified candidate,
/// one error per clause: the pool is executable and its one-way fee `φ_ppm ≤
/// venue_fee_cap_bps·100` (`PoolIneligible`); its locked depth passes the floor
/// `⌊⌊D·isqrt(P_ref)/10⁹⌋·10⁴/λ_min⌋ ≥ pool_min_real_sol` (`PoolTooThin`;
/// `shortwin_math::pool_select::locked_floor_ok`), with `P_ref = p_ref` — the market's `P_ema`
/// whenever the market has a price — or, for a market without one (a listing, a list-only
/// market that never had a pool), the candidate's own spot.
///
/// v0.4.1 read `R ≥ pool_min_real_sol` on the live SOL reserve (a buy raises it for one
/// instruction: `[buy, propose_pool, sell]` adopted a pool under the floor, M4-SEC-3) and `λ ≥
/// pool_lock_min_bps` on the live LP counter and supply (anyone's unlocked deposit lowers it for
/// as long as the deposit stays, blocking every adoption and the fast path, M4-SEC-1; the pool's
/// own LP holder raises it for one instruction). `D` moves with neither, and at `P_ema` neither
/// does the floor. On a market without a price the spot can still be pushed for one
/// instruction: such a market has no position, and opens re-check the floor on `R_eff` after
/// the listing window (ARCH §2.6 rule 5). The same clauses as `pool_select::eligible`, which the
/// keeper and the frontend run off-chain, and which must agree.
///
/// `PoolLockTooLow` is no longer returned.
///
/// This is the floor of every adoption that carries no open position (a listing, a list-only
/// market's adoption, a listing-window replacement, a zero-position adoption). A collection
/// window's candidate — whose adoption moves open positions — must also pass
/// [`position_floor_ok`] (M4V-1).
pub fn check_eligible(state: &VenueState, params: &Params, p_ref: Option<u128>) -> Result<()> {
    require!(state.executable, ShortwinError::PoolIneligible);
    require!(
        !fees::exceeds_cap(state.fee_ppm, u64::from(params.venue_fee_cap_bps)),
        ShortwinError::PoolIneligible
    );
    let p_ref = reference(state, p_ref).ok_or(ShortwinError::PoolIneligible)?;
    require!(
        pool_select::locked_floor_ok(
            state.locked_depth,
            p_ref,
            u64::from(params.pool_lock_min_bps),
            params.pool_min_real_sol,
        ),
        ShortwinError::PoolTooThin
    );
    require!(
        pool_select::eligible(
            &PoolFacts {
                executable: state.executable,
                fee_ppm: state.fee_ppm,
                locked_depth: state.locked_depth,
                p_ref,
            },
            &EligibilityParams {
                venue_fee_cap_bps: u64::from(params.venue_fee_cap_bps),
                pool_min_real_sol: params.pool_min_real_sol,
                pool_lock_min_bps: u64::from(params.pool_lock_min_bps),
            },
        ),
        ShortwinError::PoolIneligible
    );
    Ok(())
}

/// The reference price of eligibility: `p_ref` (the market's `P_ema`) when the market has a
/// price, else the candidate's own spot.
fn reference(state: &VenueState, p_ref: Option<u128>) -> Option<u128> {
    p_ref.or_else(|| state.price())
}

/// The floor of an adoption that carries open positions (M4V-1; ARCH §2.6 "Eligibility" as
/// amended; owner item O18): the candidate's **locked SOL** at the reference price itself reaches
/// `pool_min_real_sol` (`⌊D·isqrt(P_ref)/10⁹⌋ ≥ pool_min_real_sol`,
/// `shortwin_math::pool_select::position_floor_ok`: [`check_eligible`]'s floor without the
/// `/λ_min`). Applied to a collection window's candidate — a Raydium market's fast path and a
/// list-only market with open positions — when a proposal opens or improves the window
/// (`PoolTooThin`) and when `execute_pool_switch` resolves it (a cancellation). The listing floor
/// admits fully locked pools holding only `λ_min·pool_min_real_sol` (12.5 SOL), which is harmless
/// where no position exists (opens stay gated by `R_eff`, rule 5) but moved open positions onto
/// them with no manipulation at all (M4V-1). `false` without a reference price.
pub fn position_floor_ok(state: &VenueState, params: &Params, p_ref: Option<u128>) -> bool {
    reference(state, p_ref).is_some_and(|p| {
        pool_select::position_floor_ok(state.locked_depth, p, params.pool_min_real_sol)
    })
}

/// The venue CPI of a leg: `metas` (the venue's own list; `trader` its only signer) followed
/// by the resolved extras (A41; both Raydium programs ignore trailing accounts, R-RAY §9.6),
/// `trader` signing with its seeds. `infos` are the accounts of `metas` plus the venue program.
pub fn invoke_as_trader<'info>(
    leg: &Leg<'info>,
    program_id: Pubkey,
    data: Vec<u8>,
    mut metas: Vec<AccountMeta>,
    mut infos: Vec<AccountInfo<'info>>,
) -> Result<()> {
    metas.extend(leg.extra_metas.iter().cloned());
    infos.extend(leg.extra_infos.iter().cloned());
    let ix = Instruction {
        program_id,
        accounts: metas,
        data,
    };
    let bump = [leg.trader.trader_bump];
    let seeds: &[&[u8]] = &[crate::state::TRADER_SEED, leg.trader.market.as_ref(), &bump];
    invoke_signed(&ix, &infos, &[seeds])?;
    Ok(())
}

/// The sell leg of an open on Raydium (module docs): returns `S`, the proceeds now on the
/// position (which also holds the WSOL account's rent; the caller refunds the borrower that
/// rent after its last CPI naming either account). `swap` runs the venue's exact-in swap of
/// `q` from `pool_vault` (trader as delegate) into `trader_wsol`.
pub fn delegate_sell<'info>(
    leg: &Leg<'info>,
    borrower: &AccountInfo<'info>,
    position: &AccountInfo<'info>,
    q: u64,
    swap: impl FnOnce() -> Result<()>,
) -> Result<u64> {
    let market_seeds: &[&[u8]] = &[Market::SEED, leg.mint.key.as_ref(), &[leg.market_bump]];
    token_interface::approve_checked(
        CpiContext::new_with_signer(
            leg.token_program.key(),
            ApproveChecked {
                to: leg.pool_vault.clone(),
                mint: leg.mint.clone(),
                delegate: leg.trader.trader.clone(),
                authority: leg.market.clone(),
            },
            &[market_seeds],
        ),
        q,
        leg.decimals,
    )?;
    leg.trader.open_wsol(Funding::Signer(borrower), 0)?;
    let before = token_state(&leg.pool_vault)?.amount;
    swap()?;
    let after = token_state(&leg.pool_vault)?;
    require!(
        before.checked_sub(after.amount) == Some(q),
        ShortwinError::VenueResultMismatch
    );
    require!(after.delegate.is_none(), ShortwinError::DelegateNotCleared);
    let s = leg.trader.wsol_amount()?;
    leg.trader.close_wsol(position)?;
    Ok(s)
}

/// The buy leg of an exit on Raydium (module docs). `swap` runs the venue's exact-out swap of
/// `q` paid from `trader_wsol` (holding `max_in`) into `pool_vault`.
pub fn direct_buy<'info>(
    leg: &Leg<'info>,
    position: &AccountInfo<'info>,
    q: u64,
    max_in: u64,
    swap: impl FnOnce() -> Result<()>,
) -> Result<BuyOut> {
    let lamports_before = position.lamports();
    leg.trader.open_wsol(Funding::Position(position), max_in)?;
    let before = token_state(&leg.pool_vault)?.amount;
    swap()?;
    let received = token_state(&leg.pool_vault)?
        .amount
        .checked_sub(before)
        .ok_or(ShortwinError::VenueResultMismatch)?;
    require_gte!(received, q, ShortwinError::VenueResultMismatch);
    leg.trader.close_wsol(position)?;
    let spent = lamports_before.checked_sub(position.lamports()).or_math()?;
    require_gte!(max_in, spent, ShortwinError::VenueResultMismatch);
    Ok(BuyOut { spent, received })
}
