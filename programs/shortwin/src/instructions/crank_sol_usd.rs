//! `crank_sol_usd()` (ARCH §3.9, §4.7, A10, A29; C1-12): **anyone** refreshes the cached Pyth
//! SOL/USD price.
//!
//! - The Accounts constraints check the Pyth account's address (== `config.pyth_sol_usd`,
//!   `OracleFeed`) and owner (== the receiver constant `PYTH_RECEIVER_ID`, `OracleOwner`).
//! - The handler hand-parses the `PriceUpdateV2` with `shortwin_venue_state::pyth::read` and
//!   applies `pyth::check`, the rules of ARCH §3.9 in ARCH's order: `verification_level ==
//!   Full` (`OracleVerification`), feed id == `config.pyth_feed_id` and exponent in
//!   `[−18, 0]` (`OracleFeed`), `publish_time ≥ now − oracle_max_age_s` (`OracleStale`) and
//!   `≤ now + oracle_max_age_s` (`OracleFutureDated`, M1-SEC-8), `price > 0` and
//!   `conf/price ≤ oracle_max_conf_bps` (`OracleConfidence`), `|price/cached − 1| ≤
//!   oracle_max_step_bps` at a common exponent unless the cache is older than
//!   `oracle_stale_s` or empty (`OracleStep`), `publish_time > cached.publish_time`
//!   (`OracleStale`).
//! - A `PriceUpdateV2` that does not decode (wrong discriminator, truncated) is not the
//!   configured feed (`OracleFeed`); an unknown verification tag is not `Full`
//!   (`OracleVerification`).
//! - Writes `price, conf, expo, publish_time` as published and `updated_slot = clock.slot`.
//!   Nothing else is written; the cache is read only by `open_short` and `crank_price`
//!   (tiers), never by an exit (I10).
//!
//! No event: ARCH §3.10 lists none for this instruction (the indexer reads `OracleCache`).
//! Keepers retry: the sponsored feed updates on a 55 s heartbeat or a 0.5 % move, so a crank
//! often finds nothing newer than the cache (`OracleStale`).

use anchor_lang::prelude::*;
use shortwin_venue_state::pyth::{self, Cached, Limits, Refusal};
use shortwin_venue_state::ReadError;

use crate::constants::{CONFIG_VERSION, PYTH_RECEIVER_ID};
use crate::errors::ShortwinError;
use crate::state::{GlobalConfig, OracleCache};

#[derive(Accounts)]
pub struct CrankSolUsd<'info> {
    #[account(
        seeds = [GlobalConfig::SEED],
        bump = config.load()?.bump,
        constraint = config.load()?.version == CONFIG_VERSION @ ShortwinError::ConfigVersion
    )]
    pub config: AccountLoader<'info, GlobalConfig>,
    #[account(
        mut,
        seeds = [OracleCache::SEED],
        bump = oracle_cache.bump
    )]
    pub oracle_cache: Account<'info, OracleCache>,
    /// The configured Pyth `PriceUpdateV2` (address `config.pyth_sol_usd`, owner the receiver).
    /// CHECK: address and owner are constrained here; the handler parses the contents.
    #[account(
        address = config.load()?.pyth_sol_usd @ ShortwinError::OracleFeed,
        owner = PYTH_RECEIVER_ID @ ShortwinError::OracleOwner
    )]
    pub price_update: UncheckedAccount<'info>,
}

/// A `PriceUpdateV2` that does not decode.
fn read_error(e: ReadError) -> ShortwinError {
    match e {
        ReadError::WrongOwner => ShortwinError::OracleOwner,
        ReadError::TooShort | ReadError::WrongTag => ShortwinError::OracleFeed,
        ReadError::Malformed => ShortwinError::OracleVerification,
    }
}

/// An update the ARCH §3.9 rules refuse.
fn refusal(r: Refusal) -> ShortwinError {
    match r {
        Refusal::NotFull => ShortwinError::OracleVerification,
        Refusal::WrongFeed | Refusal::Exponent => ShortwinError::OracleFeed,
        Refusal::Stale | Refusal::NotNewer => ShortwinError::OracleStale,
        Refusal::FutureDated => ShortwinError::OracleFutureDated,
        Refusal::NonPositivePrice | Refusal::Confidence => ShortwinError::OracleConfidence,
        Refusal::Step => ShortwinError::OracleStep,
    }
}

pub fn handler(ctx: Context<CrankSolUsd>) -> Result<()> {
    let (feed_id, limits) = {
        let config = ctx.accounts.config.load()?;
        let p = &config.params;
        (
            config.pyth_feed_id,
            Limits {
                max_age_s: p.oracle_max_age_s,
                stale_s: p.oracle_stale_s,
                max_conf_bps: p.oracle_max_conf_bps,
                max_step_bps: p.oracle_max_step_bps,
            },
        )
    };
    let clock = Clock::get()?;

    let info = ctx.accounts.price_update.to_account_info();
    let update = {
        let data = info.try_borrow_data()?;
        pyth::read(&info.owner.to_bytes(), &data).map_err(read_error)?
    };

    let cache = &mut ctx.accounts.oracle_cache;
    let cached = Cached {
        price: cache.price,
        expo: cache.expo,
        publish_time: cache.publish_time,
    };
    pyth::check(&update, &feed_id, &cached, clock.unix_timestamp, &limits).map_err(refusal)?;

    cache.price = update.price;
    cache.conf = update.conf;
    cache.expo = update.exponent;
    cache.publish_time = update.publish_time;
    cache.updated_slot = clock.slot;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every reader failure and refusal maps to an ARCH §3.11 oracle code.
    #[test]
    fn error_mapping() {
        use ShortwinError as E;
        assert_eq!(read_error(ReadError::WrongOwner), E::OracleOwner);
        assert_eq!(read_error(ReadError::TooShort), E::OracleFeed);
        assert_eq!(read_error(ReadError::WrongTag), E::OracleFeed);
        assert_eq!(read_error(ReadError::Malformed), E::OracleVerification);
        assert_eq!(refusal(Refusal::NotFull), E::OracleVerification);
        assert_eq!(refusal(Refusal::WrongFeed), E::OracleFeed);
        assert_eq!(refusal(Refusal::Exponent), E::OracleFeed);
        assert_eq!(refusal(Refusal::Stale), E::OracleStale);
        assert_eq!(refusal(Refusal::FutureDated), E::OracleFutureDated);
        assert_eq!(refusal(Refusal::NotNewer), E::OracleStale);
        assert_eq!(refusal(Refusal::NonPositivePrice), E::OracleConfidence);
        assert_eq!(refusal(Refusal::Confidence), E::OracleConfidence);
        assert_eq!(refusal(Refusal::Step), E::OracleStep);
    }
}
