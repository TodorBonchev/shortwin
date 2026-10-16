//! Raydium AMM v4 adapter (ARCH §3.12 "RaydiumV4 account sets", §2.4.1 Raydium row, §2.6,
//! §4.1–§4.3, §5.3; R-RAY §2.1–§2.3, §3.1, §3.3, §4, §5.1, §5.3, §6.2, §9.2–§9.3). Program
//! `675kPX9…`, a native program: the first data byte is a tag.
//!
//! **Account sets** (order binding; W = writable in trades; every slot mandatory and
//! key-checked against a compile-time constant or the market's pool cache, C0-2):
//!
//! - ReadSet ([`read_keys`]): R0 `pool` (`AmmInfo`, W), R1 `coin_vault` (W), R2 `pc_vault` (W),
//!   R3 `lp_mint`. Which vault is WSOL is the market's flag `sol_is_token0_or_coin`.
//! - TradeExtras ([`trade_keys`]): X0 the AMM v4 program, X1 its authority `5Q544f…`, X2
//!   Tokenkeg, X3 the WSOL mint, X4 System, X5 `trader_wsol` (W). The base mint and its token
//!   program (Tokenkeg: AMM v4 is SPL Token only) are the instruction's named `mint` and
//!   `token_program`.
//! - CPI (8 accounts, the v2 tags only): `[Tokenkeg, pool W, authority, coin_vault W, pc_vault
//!   W, source W, destination W, trader S]`, then the extras; data `[16 | amount_in |
//!   minimum_amount_out]` (sell: source `pool_vault` with `trader` as its delegate,
//!   destination `trader_wsol`) or `[17 | max_amount_in | amount_out]` (buy: source
//!   `trader_wsol`, destination `pool_vault`). The legacy tags 9 / 11 are never used (17 / 18
//!   accounts, no extras; R-RAY §3.1, §9.6).
//!
//! **Pricing** ([`read_state`]; ARCH §4.1–§4.3): reserves `vault − need_take_pnl` per side,
//! `X` the token side and `Y` the WSOL side, real SOL `R = Y`; the fee `num/den` of the input
//! (`fees::ray_v4`, one-way `φ = ⌈num·10⁶/den⌉` ppm); quotes `quote::ray_v4`, which the dumped
//! ELF executes exactly (T-RAY-MATH, `tests/litesvm/tests/ray_math_it.rs`). The locked depth
//! `D = ⌊λ_bps·isqrt(X·Y)/10⁴⌋` with `λ` from `lp_amount` @720 and the LP mint's supply.
//!
//! **Venue predicate** (`venue_executable`, ARCH §5.3): the accounts parse and still describe
//! the adopted pool (the vaults, LP mint, nonce 254, WSOL side and token mint the market
//! recorded; the vaults' mints), the fee fraction is valid (`num < den`), and the reader's
//! predicate holds (`status ∈ {1, 6}` or `7` with `now ≥ pool_open_time`, both vaults at or
//! above `need_take_pnl`, both reserves non-zero; `shortwin_venue_state::raydium_v4`). Anything
//! else is "not executable" (A63): an admin halt or a layout change starts the S3 chain; it
//! never reverts a crank.
//!
//! **Verification** ([`verify`]; ARCH §2.6): owner and exact length 752; status ∈ {1, 6, 7};
//! nonce 254; `{coin_vault_mint, pc_vault_mint} = {WSOL, mint}`; `market.token_program ==
//! Tokenkeg`; the passed vaults and LP mint are the pool's, the vaults Tokenkeg accounts of the
//! right mints under the authority; then `eligible()`. Age from listing (O13: `pool_open_time`
//! is unusable, R-RAY §7).

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::AccountMeta;
use shortwin_math::fees::{self, RayV4Fee};
use shortwin_math::pool_select;
use shortwin_math::quote::Pricing;
use shortwin_venue_state::raydium_v4::{self, AmmInfo};
use shortwin_venue_state::token_account::TokenAccount;

use super::raydium::{
    at, cached, check_eligible, check_keys, delegate_sell, direct_buy, invoke_as_trader, lp_supply,
    sol_first, token_of, Adoption,
};
use super::{BuyOut, Leg, VenueState};
use crate::constants::{
    RAYDIUM_V4_AUTHORITY, RAYDIUM_V4_AUTHORITY_NONCE, RAYDIUM_V4_PROGRAM_ID, TOKEN_PROGRAM_ID,
    WSOL_MINT,
};
use crate::errors::ShortwinError;
use crate::state::{raydium_cache, Market, Params, VenueKind};

/// ReadSet length.
pub const READ_LEN: usize = 4;
/// TradeExtras length.
pub const TRADE_LEN: usize = 6;

/// ReadSet slots.
pub const R_POOL: usize = 0;
pub const R_COIN_VAULT: usize = 1;
pub const R_PC_VAULT: usize = 2;
pub const R_LP_MINT: usize = 3;
/// TradeExtras slots.
pub const X_PROGRAM: usize = 0;
pub const X_AUTHORITY: usize = 1;
pub const X_TOKEN_PROGRAM: usize = 2;
pub const X_WSOL_MINT: usize = 3;
pub const X_SYSTEM: usize = 4;
pub const X_TRADER_WSOL: usize = 5;

/// `SwapBaseInV2` (exact in).
pub const TAG_SWAP_BASE_IN_V2: u8 = 16;
/// `SwapBaseOutV2` (exact out).
pub const TAG_SWAP_BASE_OUT_V2: u8 = 17;

/// The ReadSet keys of `market` in order: pool, coin vault, pc vault, LP mint.
pub fn read_keys(market: &Market) -> [Pubkey; READ_LEN] {
    let (sol, tok) = (
        cached(market, raydium_cache::RAY_VAULT_SOL),
        cached(market, raydium_cache::RAY_VAULT_TOK),
    );
    let (coin, pc) = if sol_first(market) {
        (sol, tok)
    } else {
        (tok, sol)
    };
    [
        cached(market, raydium_cache::RAY_POOL),
        coin,
        pc,
        cached(market, raydium_cache::RAY_LP_MINT),
    ]
}

/// The TradeExtras keys of `market` in order (X5 `trader_wsol` is writable in the CPI).
pub fn trade_keys(market: &Market) -> [Pubkey; TRADE_LEN] {
    [
        RAYDIUM_V4_PROGRAM_ID,
        RAYDIUM_V4_AUTHORITY,
        TOKEN_PROGRAM_ID,
        WSOL_MINT,
        anchor_lang::system_program::ID,
        market.trader_wsol,
    ]
}

/// Key-checks the ReadSet against the market's pool cache.
pub fn check_read(market: &Market, read: &[AccountInfo]) -> Result<()> {
    check_keys(read, &read_keys(market))
}

/// Key-checks the TradeExtras against the compile-time constants and the market; the program
/// slot must be executable.
pub fn check_trade(market: &Market, trade: &[AccountInfo]) -> Result<()> {
    check_keys(trade, &trade_keys(market))?;
    require!(
        at(trade, X_PROGRAM)?.executable,
        ShortwinError::VenueAccountMismatch
    );
    Ok(())
}

/// A parsed ReadSet: the pool, its two vaults (Tokenkeg accounts of the pool's mints, keyed as
/// the pool records them) and the LP mint's supply.
struct Parsed {
    info: AmmInfo,
    coin: TokenAccount,
    pc: TokenAccount,
    lp_supply: u64,
}

/// The pool account parsed (owner AMM v4, exactly 752 bytes).
fn pool_info(info: &AccountInfo) -> Option<AmmInfo> {
    let data = info.try_borrow_data().ok()?;
    raydium_v4::read(&info.owner.to_bytes(), &data).ok()
}

fn parse(read: &[AccountInfo]) -> Option<Parsed> {
    let (pool, coin_i, pc_i, lp_i) = (
        read.get(R_POOL)?,
        read.get(R_COIN_VAULT)?,
        read.get(R_PC_VAULT)?,
        read.get(R_LP_MINT)?,
    );
    let info = pool_info(pool)?;
    if info.coin_vault != coin_i.key.to_bytes()
        || info.pc_vault != pc_i.key.to_bytes()
        || info.lp_mint != lp_i.key.to_bytes()
    {
        return None;
    }
    let coin = token_of(coin_i, &TOKEN_PROGRAM_ID)?;
    let pc = token_of(pc_i, &TOKEN_PROGRAM_ID)?;
    if coin.mint != info.coin_vault_mint || pc.mint != info.pc_vault_mint {
        return None;
    }
    Some(Parsed {
        info,
        coin,
        pc,
        lp_supply: lp_supply(lp_i)?,
    })
}

/// The state of an executable pool (module docs), `None` otherwise.
fn state_of(p: &Parsed, now: i64) -> Option<VenueState> {
    let sol_is_coin = p.info.sol_is_coin()?;
    let fee = fees::ray_v4(p.info.swap_fee_numerator, p.info.swap_fee_denominator).ok()?;
    if !p.info.venue_executable(now, p.coin.amount, p.pc.amount) {
        return None;
    }
    let (coin, pc) = p.info.reserves(p.coin.amount, p.pc.amount)?;
    let (x, y) = if sol_is_coin { (pc, coin) } else { (coin, pc) };
    let locked_depth = pool_select::locked_depth(p.info.lp_amount, p.lp_supply, x, y).ok()?;
    Some(VenueState {
        kind: VenueKind::RaydiumV4,
        executable: true,
        x,
        y,
        r: y,
        pricing: Some(Pricing::RayV4(fee)),
        fee_ppm: fee.oneway_ppm(),
        locked_depth,
        curve: None,
        pool: None,
    })
}

/// The pool still describes the market's adoption: nonce 254 (authority `5Q544f…`), the WSOL
/// side the market recorded, the market's mint on the other side, an SPL Token market.
fn consistent(market: &Market, info: &AmmInfo) -> bool {
    let sol_is_coin = sol_first(market);
    let tok_mint = if sol_is_coin {
        info.pc_vault_mint
    } else {
        info.coin_vault_mint
    };
    info.nonce == u64::from(RAYDIUM_V4_AUTHORITY_NONCE)
        && info.sol_is_coin() == Some(sol_is_coin)
        && tok_mint == market.mint.to_bytes()
        && market.token_program == TOKEN_PROGRAM_ID
}

/// The venue state from a key-checked ReadSet at clock time `now` (module docs). Never fails:
/// whatever does not parse or no longer matches the adoption is "not executable" (A63).
pub fn read_state(market: &Market, read: &[AccountInfo], now: i64) -> VenueState {
    parse(read)
        .filter(|p| consistent(market, &p.info))
        .and_then(|p| state_of(&p, now))
        .unwrap_or_else(|| VenueState::not_executable(VenueKind::RaydiumV4))
}

/// The fee view for risk quotes whether or not the pool can execute (`super::risk_pricing`):
/// the pool's `num/den` while the pool account parses with a valid fraction (a halted pool
/// keeps its fee), else the cap itself (`φ_live` unknown: a layout change); capped at
/// `cap_bps`. Reads only the key-checked pool slot.
pub fn fee_view(read: &[AccountInfo], cap_bps: u64) -> Pricing {
    let live = read
        .get(R_POOL)
        .and_then(pool_info)
        .and_then(|i| fees::ray_v4(i.swap_fee_numerator, i.swap_fee_denominator).ok());
    let fee = live.unwrap_or(RayV4Fee {
        num: cap_bps,
        den: shortwin_math::num::BPS,
    });
    Pricing::RayV4(fee).capped(cap_bps)
}

/// `create_market(venue_hint = RaydiumV4)` and pool selection: verifies the candidate ReadSet
/// `read` for `mint` under `token_program` (module docs, ARCH §2.6) and checks `eligible()` at
/// the reference price `p_ref` (`raydium::check_eligible`: the market's `P_ema`, or `None` for
/// the candidate's own spot on a market without a price).
///
/// Errors: `BadRemainingAccounts` (not exactly the ReadSet); `PoolIneligible` (not an AMM v4
/// pool of 752 bytes, a status outside {1, 6, 7}, another nonce, not a WSOL pool of `mint`, a
/// Token-2022 market, vaults that are not the authority's Tokenkeg accounts of the pool's
/// mints, an LP mint that does not parse, a pool that cannot execute or whose fee is above
/// `venue_fee_cap_bps`); `VenueAccountMismatch` (a vault or the LP mint is not the pool's);
/// `PoolTooThin` (the locked depth under the floor, M4-SEC-1 / M4-SEC-3).
pub fn verify(
    read: &[AccountInfo],
    mint: &Pubkey,
    token_program: &Pubkey,
    params: &Params,
    now: i64,
    p_ref: Option<u128>,
) -> Result<Adoption> {
    require!(read.len() == READ_LEN, ShortwinError::BadRemainingAccounts);
    let pool = at(read, R_POOL)?;
    require_keys_eq!(
        *pool.owner,
        RAYDIUM_V4_PROGRAM_ID,
        ShortwinError::PoolIneligible
    );
    require!(
        pool.data_len() == raydium_v4::LEN,
        ShortwinError::PoolIneligible
    );
    let info = pool_info(pool).ok_or(ShortwinError::PoolIneligible)?;
    require!(
        matches!(
            info.status,
            raydium_v4::STATUS_INITIALIZED
                | raydium_v4::STATUS_SWAP_ONLY
                | raydium_v4::STATUS_WAITING_TRADE
        ),
        ShortwinError::PoolIneligible
    );
    require!(
        info.nonce == u64::from(RAYDIUM_V4_AUTHORITY_NONCE),
        ShortwinError::PoolIneligible
    );
    let sol_is_coin = info.sol_is_coin().ok_or(ShortwinError::PoolIneligible)?;
    let tok_mint = if sol_is_coin {
        info.pc_vault_mint
    } else {
        info.coin_vault_mint
    };
    require!(tok_mint == mint.to_bytes(), ShortwinError::PoolIneligible);
    require_keys_eq!(
        *token_program,
        TOKEN_PROGRAM_ID,
        ShortwinError::PoolIneligible
    );
    let (coin_i, pc_i, lp_i) = (
        at(read, R_COIN_VAULT)?,
        at(read, R_PC_VAULT)?,
        at(read, R_LP_MINT)?,
    );
    require!(
        coin_i.key.to_bytes() == info.coin_vault
            && pc_i.key.to_bytes() == info.pc_vault
            && lp_i.key.to_bytes() == info.lp_mint,
        ShortwinError::VenueAccountMismatch
    );
    let authority = RAYDIUM_V4_AUTHORITY.to_bytes();
    for (vault, want_mint) in [(coin_i, info.coin_vault_mint), (pc_i, info.pc_vault_mint)] {
        let t = token_of(vault, &TOKEN_PROGRAM_ID).ok_or(ShortwinError::PoolIneligible)?;
        require!(
            t.mint == want_mint && t.owner == authority,
            ShortwinError::PoolIneligible
        );
    }
    let parsed = parse(read).ok_or(ShortwinError::PoolIneligible)?;
    let state = state_of(&parsed, now).ok_or(ShortwinError::PoolIneligible)?;
    check_eligible(&state, params, p_ref)?;
    let (vault_sol, vault_tok) = if sol_is_coin {
        (coin_i.key(), pc_i.key())
    } else {
        (pc_i.key(), coin_i.key())
    };
    Ok(Adoption {
        kind: VenueKind::RaydiumV4,
        pool: pool.key(),
        vault_sol,
        vault_tok,
        amm_config: Pubkey::default(),
        observation: Pubkey::default(),
        lp_mint: lp_i.key(),
        tok_program: TOKEN_PROGRAM_ID,
        sol_first: sol_is_coin,
        state,
        age_start: now,
    })
}

/// The swap CPI (module docs): `tag` 16 `(amount_in, minimum_amount_out)` or 17
/// `(max_amount_in, amount_out)`, from `source` to `destination`, `trader` signing.
fn swap<'info>(
    leg: &Leg<'info>,
    tag: u8,
    a: u64,
    b: u64,
    source: &AccountInfo<'info>,
    destination: &AccountInfo<'info>,
) -> Result<()> {
    let pool = at(leg.read, R_POOL)?;
    let coin = at(leg.read, R_COIN_VAULT)?;
    let pc = at(leg.read, R_PC_VAULT)?;
    let program = at(leg.trade, X_PROGRAM)?;
    let authority = at(leg.trade, X_AUTHORITY)?;
    let token = at(leg.trade, X_TOKEN_PROGRAM)?;
    let trader = &leg.trader.trader;
    let metas = vec![
        AccountMeta::new_readonly(token.key(), false),
        AccountMeta::new(pool.key(), false),
        AccountMeta::new_readonly(authority.key(), false),
        AccountMeta::new(coin.key(), false),
        AccountMeta::new(pc.key(), false),
        AccountMeta::new(source.key(), false),
        AccountMeta::new(destination.key(), false),
        AccountMeta::new_readonly(trader.key(), true),
    ];
    let infos = vec![
        token.clone(),
        pool.clone(),
        authority.clone(),
        coin.clone(),
        pc.clone(),
        source.clone(),
        destination.clone(),
        trader.clone(),
        program.clone(),
    ];
    let mut data = Vec::with_capacity(17);
    data.push(tag);
    data.extend_from_slice(&a.to_le_bytes());
    data.extend_from_slice(&b.to_le_bytes());
    invoke_as_trader(leg, RAYDIUM_V4_PROGRAM_ID, data, metas, infos)
}

/// The sell leg of an open (ARCH §2.4.1; `super::raydium::delegate_sell`): `swap_base_in_v2(q,
/// min_out)` from `pool_vault` (trader as delegate for exactly `q`) into `trader_wsol`.
pub fn sell<'info>(
    leg: &Leg<'info>,
    borrower: &AccountInfo<'info>,
    position: &AccountInfo<'info>,
    q: u64,
    min_out: u64,
) -> Result<u64> {
    delegate_sell(leg, borrower, position, q, || {
        swap(
            leg,
            TAG_SWAP_BASE_IN_V2,
            q,
            min_out,
            &leg.pool_vault,
            leg.trader.trader_wsol()?,
        )
    })
}

/// The buy leg of an exit (ARCH §2.4.1; `super::raydium::direct_buy`): `swap_base_out_v2(max_in,
/// q)` from `trader_wsol` straight into `pool_vault`.
pub fn buy<'info>(
    leg: &Leg<'info>,
    position: &AccountInfo<'info>,
    q: u64,
    max_in: u64,
) -> Result<BuyOut> {
    direct_buy(leg, position, q, max_in, || {
        swap(
            leg,
            TAG_SWAP_BASE_OUT_V2,
            max_in,
            q,
            leg.trader.trader_wsol()?,
            &leg.pool_vault,
        )
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;
    use std::str::FromStr;

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(
                "../../../../tests/fixtures/accounts/raydium/",
                $name,
                ".bin"
            ))
        };
    }

    fn key(s: &str) -> Pubkey {
        Pubkey::from_str(s).unwrap()
    }

    /// The Fartcoin pool's ReadSet as mainnet held it (`tests/fixtures/manifest.json`, group
    /// `raydium_state`): `(key, owner, data)` of pool, coin vault, pc vault, LP mint.
    fn fartcoin() -> Vec<(Pubkey, Pubkey, Vec<u8>)> {
        vec![
            (
                key("Bzc9NZfMqkXR6fz1DBph7BDf9BroyEf6pnzESP7v5iiw"),
                RAYDIUM_V4_PROGRAM_ID,
                fixture!("ray_v4_fartcoin_pool").to_vec(),
            ),
            (
                key("F6iWqisguZYprVwp916BgGR7d5ahP6Ev5E213k8y3MEb"),
                TOKEN_PROGRAM_ID,
                fixture!("ray_v4_fartcoin_coin_vault").to_vec(),
            ),
            (
                key("7bxbfwXi1CY7zWUXW35PBMZjhPD27SarVuHaehMzR2Fn"),
                TOKEN_PROGRAM_ID,
                fixture!("ray_v4_fartcoin_pc_vault").to_vec(),
            ),
            (
                key("6JDPHdZEgQ5YtercMxNiSvVtVTkvWADvxA2x6xR3XHC8"),
                TOKEN_PROGRAM_ID,
                fixture!("ray_v4_fartcoin_lp_mint").to_vec(),
            ),
        ]
    }

    const FARTCOIN_MINT: &str = "9BB6NFEcjBCtnNLFko2FqVQBq8HHM13kCyYcdQbgpump";
    /// The snapshot's block time.
    const NOW: i64 = 1_790_563_273;

    /// Runs `f` on the ReadSet `accounts` as `AccountInfo`s.
    fn with_infos<R>(
        accounts: &mut [(Pubkey, Pubkey, Vec<u8>)],
        f: impl FnOnce(&[AccountInfo]) -> R,
    ) -> R {
        let mut lamports: Vec<u64> = vec![1; accounts.len()];
        let infos: Vec<AccountInfo> = accounts
            .iter_mut()
            .zip(lamports.iter_mut())
            .map(|((k, o, d), l)| AccountInfo::new(k, false, false, l, d.as_mut_slice(), o, false))
            .collect();
        f(&infos)
    }

    #[test]
    fn verifies_adopts_and_reads_fartcoin() {
        let mint = key(FARTCOIN_MINT);
        let params = Params::defaults();
        let mut accounts = fartcoin();
        let a = with_infos(&mut accounts, |r| {
            verify(r, &mint, &TOKEN_PROGRAM_ID, &params, NOW, None)
        })
        .unwrap();
        assert_eq!(a.kind, VenueKind::RaydiumV4);
        assert!(a.sol_first, "WSOL is the coin");
        assert_eq!(a.vault_sol, accounts[1].0);
        assert_eq!(a.vault_tok, accounts[2].0);
        assert_eq!(a.age_start, NOW, "AMM v4 ages from the listing (O13)");
        // The manifest's vault amounts: WSOL 38,257,803,921,700; FART 24,095,698,311,693.
        assert_eq!(
            (a.state.x, a.state.y),
            (24_095_698_311_693, 38_257_803_921_700)
        );
        assert_eq!((a.state.r, a.state.fee_ppm), (a.state.y, 2_500));
        let counter = u64::from_le_bytes(accounts[0].2[720..728].try_into().unwrap());
        let supply = u64::from_le_bytes(accounts[3].2[36..44].try_into().unwrap());
        assert_eq!(
            a.state.locked_depth,
            shortwin_math::pool_select::locked_depth(counter, supply, a.state.x, a.state.y)
                .unwrap(),
            "D from lp_amount @720 and the LP mint's supply"
        );
        assert_eq!(
            shortwin_math::pool_select::lambda_bps(counter, supply),
            9_978
        );
        let mut m = Market::zeroed();
        m.mint = mint;
        m.token_program = TOKEN_PROGRAM_ID;
        a.apply(&mut m, NOW).unwrap();
        assert_eq!(
            read_keys(&m).to_vec(),
            accounts.iter().map(|(k, _, _)| *k).collect::<Vec<_>>()
        );
        let st = with_infos(&mut accounts, |r| {
            check_read(&m, r).unwrap();
            read_state(&m, r, NOW)
        });
        assert!(st.executable);
        assert_eq!(
            (st.x, st.y, st.locked_depth),
            (a.state.x, a.state.y, a.state.locked_depth)
        );
        assert_eq!(m.pool_d.get(), st.locked_depth);
    }

    #[test]
    fn halts_and_layout_breaks_are_not_executable() {
        let mint = key(FARTCOIN_MINT);
        let mut m = Market::zeroed();
        m.mint = mint;
        m.token_program = TOKEN_PROGRAM_ID;
        let mut accounts = fartcoin();
        let a = with_infos(&mut accounts, |r| {
            verify(r, &mint, &TOKEN_PROGRAM_ID, &Params::defaults(), NOW, None)
        })
        .unwrap();
        a.apply(&mut m, NOW).unwrap();
        for status in [0u64, 2, 3, 4, 5, 8] {
            let mut acc = fartcoin();
            acc[0].2[..8].copy_from_slice(&status.to_le_bytes());
            let st = with_infos(&mut acc, |r| read_state(&m, r, NOW));
            assert!(!st.executable, "status {status}");
            assert_eq!(st.fee_ppm, 0);
            // The fee view still reads the halted pool's fee (capped).
            let f = with_infos(&mut acc, |r| fee_view(r, 500));
            assert_eq!(f.oneway_ppm(), 2_500);
        }
        // Another length (the venue loads exactly 752 bytes), another owner, a moved vault.
        let mut acc = fartcoin();
        acc[0].2.push(0);
        assert!(!with_infos(&mut acc, |r| read_state(&m, r, NOW)).executable);
        assert_eq!(
            with_infos(&mut acc, |r| fee_view(r, 500)).oneway_ppm(),
            50_000,
            "the cap"
        );
        let mut acc = fartcoin();
        acc[1].1 = crate::constants::TOKEN_2022_PROGRAM_ID;
        assert!(!with_infos(&mut acc, |r| read_state(&m, r, NOW)).executable);
        let mut acc = fartcoin();
        acc[0].2[raydium_v4::OFF_NONCE] = 253;
        assert!(!with_infos(&mut acc, |r| read_state(&m, r, NOW)).executable);
        // A market recording the other side as WSOL.
        let mut other = m;
        other.set_flag(crate::state::MARKET_FLAG_SOL_IS_TOKEN0_OR_COIN, false);
        let mut acc = fartcoin();
        assert!(!with_infos(&mut acc, |r| read_state(&other, r, NOW)).executable);
    }
}
