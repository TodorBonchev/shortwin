//! The account refresher, the venue admin watch and token metadata (INDEXER.md §2.6, §7.3).
//! Owner: INGEST.
//!
//! - [`write_snapshot`]: any short.win account (decoded with the IDL, §4.2) → `account_snapshots`
//!   (latest slot wins), then its consequences: a `Market` → [`refresh_market_from_snapshot`]
//!   (`markets.coin_creator` from `cached_creator` / the PumpSwap pool cache's
//!   `CACHED_COIN_CREATOR`, and the price columns when the snapshot is newer than the last price
//!   event, `price_source = 'account'`); an `OracleCache` → `oracle_prices`; a `ShortPosition` →
//!   the position's locked terms when the parameters history could not give them (§6.5).
//! - Live ([`run`]): after a confirmed slot with events of a market (at most every
//!   `refresher.market_min_interval_ms` per market) `getMultipleAccounts([market, market_vault])`;
//!   new positions; every `refresher.oracle_interval_s` the `OracleCache`; every
//!   `refresher.venue_watch_interval_s` the venue admin accounts the adapters read — each
//!   Raydium market pool's status and its CPMM `AmmConfig`'s rates (a changed watched field →
//!   `venue_admin_states`; the writer reads the CPMM protocol and fund rates back for the swaps'
//!   pricing reserves); token metadata (Metaplex metadata PDA, else decimals
//!   only) at `MarketCreated`.
//! - Replay: a session's account checkpoints play this role ([`apply_checkpoint`]).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Context;
use base64::Engine;
use serde_json::Value;
use solana_pubkey::Pubkey;
use sqlx::{PgConnection, PgPool, Row};

use super::decode::shortwin_events::{i64_field, key_field, u32_field, u64_field, u128_field};
use super::decode::{idl, venue};
use super::session::AccountsCheckpoint;
use super::store::slot_i64;

/// Writes one short.win account snapshot and its consequences (module docs). `live_prices`:
/// a `Market` snapshot also overrides the price columns when newer than the last price event
/// (the live refresher; a replayed checkpoint and a rebuild leave the price columns
/// event-derived, so they stay a function of the events). Returns the decoded account type, or
/// `None` for data that is no short.win account.
pub async fn write_snapshot(
    conn: &mut PgConnection,
    pubkey: &Pubkey,
    lamports: u64,
    data: &[u8],
    slot: u64,
    live_prices: bool,
) -> anyhow::Result<Option<String>> {
    let Ok((ty, json)) = idl::decode_account(data) else {
        return Ok(None);
    };
    if !matches!(
        ty.as_str(),
        "GlobalConfig"
            | "VenueGuard"
            | "VenueExtras"
            | "OracleCache"
            | "Market"
            | "MarketVault"
            | "ShortPosition"
            | "LenderPosition"
            | "ShareEpoch"
            | "WalletExposure"
    ) {
        return Ok(None);
    }
    let market = match ty.as_str() {
        "Market" => Some(*pubkey),
        _ => key_field(&json, "market"),
    };
    sqlx::query(
        "INSERT INTO account_snapshots (pubkey, account_type, market, lamports, slot, data) \
         VALUES ($1, $2, $3, $4::numeric, $5, $6) \
         ON CONFLICT (pubkey) DO UPDATE SET account_type = EXCLUDED.account_type, \
           market = EXCLUDED.market, lamports = EXCLUDED.lamports, slot = EXCLUDED.slot, \
           data = EXCLUDED.data, fetched_at = now() \
         WHERE EXCLUDED.slot >= account_snapshots.slot",
    )
    .bind(pubkey.to_string())
    .bind(&ty)
    .bind(market.map(|m| m.to_string()))
    .bind(lamports.to_string())
    .bind(slot_i64(slot))
    .bind(&json)
    .execute(&mut *conn)
    .await
    .context("account_snapshots")?;
    match ty.as_str() {
        "Market" => refresh_market_from_snapshot(conn, pubkey, live_prices).await?,
        "OracleCache" => {
            sqlx::query(
                "INSERT INTO oracle_prices (publish_time, price, conf, expo, updated_slot) \
                 VALUES ($1, $2, $3::numeric, $4, $5) ON CONFLICT (publish_time) DO NOTHING",
            )
            .bind(i64_field(&json, "publish_time"))
            .bind(i64_field(&json, "price"))
            .bind(u64_field(&json, "conf").to_string())
            .bind(
                json.get("expo")
                    .and_then(Value::as_i64)
                    .and_then(|v| i32::try_from(v).ok())
                    .unwrap_or(0),
            )
            .bind(slot_i64(u64_field(&json, "updated_slot")))
            .execute(&mut *conn)
            .await?;
        }
        "ShortPosition" => {
            let t = super::project::terms_of_position_account(&json);
            sqlx::query(
                "UPDATE positions SET rent_protocol_bps = $2, penalty_lender_bps = $3, \
                   penalty_protocol_bps = $4, penalty_keeper_bps = $5, expiry_fee_bps = $6, \
                   terms_source = 'account' WHERE position = $1 AND terms_source IS NULL",
            )
            .bind(pubkey.to_string())
            .bind(i32::from(t[0]))
            .bind(i32::from(t[1]))
            .bind(i32::from(t[2]))
            .bind(i32::from(t[3]))
            .bind(i32::from(t[4]))
            .execute(&mut *conn)
            .await?;
        }
        _ => {}
    }
    Ok(Some(ty))
}

/// The account-sourced [A] columns of `markets` from the latest `Market` snapshot (§2.6, §6.2):
/// `coin_creator` (pump curve: `cached_creator`; PumpSwap: the pool cache's
/// `CACHED_COIN_CREATOR`), and, with `live_prices`, the price columns when the snapshot is newer
/// than the last price event (`price_source = 'account'`). A rebuild calls it without prices
/// (the next refresher tick re-applies them, [`refresh_live_prices`]).
pub async fn refresh_market_from_snapshot(
    conn: &mut PgConnection,
    market: &Pubkey,
    live_prices: bool,
) -> anyhow::Result<()> {
    let Some(r) = sqlx::query(
        "SELECT slot, data FROM account_snapshots WHERE pubkey = $1 AND account_type = 'Market'",
    )
    .bind(market.to_string())
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(());
    };
    let slot: i64 = r.try_get("slot")?;
    let d: Value = r.try_get("data")?;
    let venue = u32_field(&d, "venue");
    let coin_creator = match venue {
        1 | 2 => key_field(&d, "cached_creator"),
        3 => d
            .get("pool_cache")
            .and_then(|a| a.get(shortwin::state::pump_swap_cache::CACHED_COIN_CREATOR))
            .and_then(Value::as_str)
            .and_then(|s| s.parse().ok()),
        _ => None,
    };
    if let Some(cc) = coin_creator {
        sqlx::query(
            "UPDATE markets SET coin_creator = $2, coin_creator_source = 'account' WHERE market = $1",
        )
        .bind(market.to_string())
        .bind(cc.to_string())
        .execute(&mut *conn)
        .await?;
    }
    if !live_prices {
        return Ok(());
    }
    let basis = |i: usize| -> Option<String> {
        d.get("rent_basis")
            .and_then(|a| a.get(i))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let util = crate::numeric::util_bps(u64_field(&d, "tokens_free"), u64_field(&d, "borrowed_q"));
    sqlx::query(
        "UPDATE markets SET ema_price = $3::numeric, ema_liq = $4::numeric, liq_slow = $5::numeric, \
           ema_count = $6::numeric, lb_lo = $7::numeric, lb_hi = $8::numeric, tier = $9, \
           vest_bal = $10::numeric, rent_basis0 = $11::numeric, rent_basis1 = $12::numeric, \
           rent_basis2 = $13::numeric, util_bps = $14, price_slot = $2, price_ts = $15, \
           price_source = 'account' \
         WHERE market = $1 AND $2 > COALESCE(price_slot, -1)",
    )
    .bind(market.to_string())
    .bind(slot)
    .bind(u128_field(&d, "ema_price").to_string())
    .bind(u128_field(&d, "ema_liq").to_string())
    .bind(u128_field(&d, "liq_slow").to_string())
    .bind(u64_field(&d, "ema_count").to_string())
    .bind(u128_field(&d, "lb_lo").to_string())
    .bind(u128_field(&d, "lb_hi").to_string())
    .bind(i16::try_from(u32_field(&d, "tier")).unwrap_or(0))
    .bind(u64_field(&d, "vest_bal").to_string())
    .bind(basis(0))
    .bind(basis(1))
    .bind(basis(2))
    .bind(i64::from(util))
    .bind(i64_field(&d, "ema_ts"))
    .execute(&mut *conn)
    .await
    .context("markets (account price)")?;
    Ok(())
}

/// Re-applies the live price override of every market whose latest `Market` snapshot is newer
/// than its last price (after a rebuild, which leaves the price columns event-derived).
pub async fn refresh_live_prices(pool: &PgPool) -> anyhow::Result<()> {
    let markets: Vec<String> = sqlx::query_scalar(
        "SELECT m.market::text FROM markets m JOIN account_snapshots s ON s.pubkey = m.market \
         WHERE s.account_type = 'Market' AND s.slot > COALESCE(m.price_slot, -1)",
    )
    .fetch_all(pool)
    .await?;
    for m in markets {
        let mut conn = pool.acquire().await?;
        refresh_market_from_snapshot(&mut conn, &crate::numeric::parse_key(&m)?, true).await?;
    }
    Ok(())
}

/// A replayed session's checkpoint in the refresher's role (§2.5): every short.win account of
/// the checkpoint is written as the live refresher would write it, except the live price
/// override (a replay's price columns stay event-derived).
pub async fn apply_checkpoint(
    pool: &PgPool,
    cp: &AccountsCheckpoint,
    program: &Pubkey,
) -> anyhow::Result<u64> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut tx = pool.begin().await?;
    let mut n = 0;
    // Markets after their dependents: a position's terms before a market's price refresh is
    // irrelevant, but keep a deterministic order.
    let mut accounts: Vec<&super::session::SessionAccount> = cp.accounts.iter().collect();
    accounts.sort_by(|a, b| a.pubkey.cmp(&b.pubkey));
    for a in accounts {
        if a.owner != program.to_string() {
            continue;
        }
        let data = b64
            .decode(&a.data)
            .with_context(|| format!("checkpoint account {}", a.pubkey))?;
        let key = crate::numeric::parse_key(&a.pubkey)?;
        if write_snapshot(&mut tx, &key, a.lamports, &data, cp.slot, false)
            .await?
            .is_some()
        {
            n += 1;
        }
    }
    tx.commit().await?;
    Ok(n)
}

// ---- live ------------------------------------------------------------------------------------

/// The live refresher's state (module docs).
pub struct Refresher {
    client: solana_rpc_client::nonblocking::rpc_client::RpcClient,
    /// The endpoint's credentials never reach a log (M8-SEC-15).
    redactor: super::rpc::Redactor,
    cfg: super::config::RefresherConfig,
    program: Pubkey,
    last_market: HashMap<Pubkey, Instant>,
    last_oracle: Option<Instant>,
    last_watch: Option<Instant>,
    watched: AdminValues,
}

impl Refresher {
    pub fn new(rpc_url: &str, cfg: super::config::RefresherConfig, program: Pubkey) -> Self {
        Self {
            client: solana_rpc_client::nonblocking::rpc_client::RpcClient::new_with_commitment(
                rpc_url.to_string(),
                solana_commitment_config::CommitmentConfig::confirmed(),
            ),
            redactor: super::rpc::Redactor::new(rpc_url),
            cfg,
            program,
            last_market: HashMap::new(),
            last_oracle: None,
            last_watch: None,
            watched: HashMap::new(),
        }
    }

    async fn read_and_write(&self, pool: &PgPool, keys: &[Pubkey]) -> anyhow::Result<u64> {
        if keys.is_empty() {
            return Ok(0);
        }
        let res = self
            .client
            .get_multiple_accounts_with_commitment(
                keys,
                solana_commitment_config::CommitmentConfig::confirmed(),
            )
            .await
            .context("getMultipleAccounts")?;
        let slot = res.context.slot;
        let mut tx = pool.begin().await?;
        let mut n = 0;
        for (k, acc) in keys.iter().zip(res.value) {
            let Some(acc) = acc else { continue };
            if acc.owner != self.program {
                continue;
            }
            if write_snapshot(&mut tx, k, acc.lamports, &acc.data, slot, true)
                .await?
                .is_some()
            {
                n += 1;
            }
        }
        tx.commit().await?;
        Ok(n)
    }

    /// One refresher pass: markets with recent events, new positions without terms, the oracle,
    /// token metadata, the venue admin watch, and the account prices a rebuild left out.
    pub async fn tick(&mut self, pool: &PgPool) -> anyhow::Result<()> {
        if !self.cfg.enabled {
            return Ok(());
        }
        refresh_live_prices(pool).await?;
        let min = Duration::from_millis(self.cfg.market_min_interval_ms);
        let recent: Vec<(String, Option<String>)> = sqlx::query_as(
            "SELECT m.market::text, v.pubkey::text FROM markets m \
             LEFT JOIN account_snapshots v ON v.market = m.market AND v.account_type = 'MarketVault' \
             WHERE m.last_slot > COALESCE((SELECT slot FROM account_snapshots s WHERE s.pubkey = m.market), -1)",
        )
        .fetch_all(pool)
        .await?;
        let mut keys = Vec::new();
        for (m, vault) in recent {
            let m = crate::numeric::parse_key(&m)?;
            if self.last_market.get(&m).is_some_and(|t| t.elapsed() < min) {
                continue;
            }
            self.last_market.insert(m, Instant::now());
            keys.push(m);
            let vault = match vault {
                Some(v) => crate::numeric::parse_key(&v)?,
                None => {
                    Pubkey::find_program_address(&[b"market_vault", m.as_ref()], &self.program).0
                }
            };
            keys.push(vault);
        }
        // Positions whose terms the parameters history could not give.
        let open: Vec<String> = sqlx::query_scalar(
            "SELECT position::text FROM positions WHERE terms_source IS NULL AND status = 'open' LIMIT 100",
        )
        .fetch_all(pool)
        .await?;
        for p in open {
            keys.push(crate::numeric::parse_key(&p)?);
        }
        if self
            .last_oracle
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(self.cfg.oracle_interval_s))
        {
            self.last_oracle = Some(Instant::now());
            keys.push(
                Pubkey::find_program_address(&[shortwin::state::OracleCache::SEED], &self.program)
                    .0,
            );
        }
        for chunk in keys.chunks(100) {
            self.read_and_write(pool, chunk).await?;
        }
        if self.cfg.token_metadata {
            self.token_metadata(pool).await?;
        }
        if self
            .last_watch
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(self.cfg.venue_watch_interval_s))
        {
            self.last_watch = Some(Instant::now());
            if let Err(e) = self.venue_watch(pool).await {
                tracing::warn!(error = %self.redactor.redact(&format!("{e:#}")), "venue admin watch");
            }
        }
        Ok(())
    }

    /// `token_metadata` for markets without a row: the mint's decimals and, when present, the
    /// Metaplex metadata account's name / symbol / uri (length-capped, §10.3).
    async fn token_metadata(&self, pool: &PgPool) -> anyhow::Result<()> {
        let mints: Vec<String> = sqlx::query_scalar(
            "SELECT m.mint::text FROM markets m LEFT JOIN token_metadata t ON t.mint = m.mint \
             WHERE t.mint IS NULL LIMIT 50",
        )
        .fetch_all(pool)
        .await?;
        let metaplex = solana_pubkey::pubkey!("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s");
        for m in mints {
            let mint = crate::numeric::parse_key(&m)?;
            let meta = Pubkey::find_program_address(
                &[b"metadata", metaplex.as_ref(), mint.as_ref()],
                &metaplex,
            )
            .0;
            let res = self
                .client
                .get_multiple_accounts_with_commitment(
                    &[mint, meta],
                    solana_commitment_config::CommitmentConfig::confirmed(),
                )
                .await?;
            let slot = res.context.slot;
            let mut it = res.value.into_iter();
            let mint_acc = it.next().flatten();
            let meta_acc = it.next().flatten();
            let decimals = mint_acc
                .as_ref()
                .and_then(|a| a.data.get(44).copied())
                .unwrap_or(0);
            let parsed = meta_acc.as_ref().and_then(|a| metaplex_strings(&a.data));
            insert_token_metadata(pool, &mint, decimals, parsed, slot).await?;
        }
        Ok(())
    }

    /// The venue admin watch (§7.3): the status / fee fields the adapters read (AMM v4 and CPMM
    /// pool status, then the CPMM pools' `AmmConfig` rates); a changed value →
    /// `venue_admin_states`.
    async fn venue_watch(&mut self, pool: &PgPool) -> anyhow::Result<()> {
        let pools: Vec<(String, i16)> = sqlx::query_as(
            "SELECT ray_pool::text, venue::smallint FROM markets WHERE ray_pool IS NOT NULL AND venue IN (4, 5)",
        )
        .fetch_all(pool)
        .await?;
        let keys: Vec<Pubkey> = pools.iter().filter_map(|(p, _)| p.parse().ok()).collect();
        if keys.is_empty() {
            return Ok(());
        }
        let res = self
            .client
            .get_multiple_accounts_with_commitment(
                &keys,
                solana_commitment_config::CommitmentConfig::confirmed(),
            )
            .await?;
        let slot = res.context.slot;
        let mut configs: Vec<Pubkey> = Vec::new();
        for ((key, (_, venue)), acc) in keys.iter().zip(&pools).zip(res.value) {
            let Some(acc) = acc else { continue };
            let owner = acc.owner.to_bytes();
            let (fields, orientation): (WatchedFields, Option<(Pubkey, bool)>) = if *venue == 4 {
                match shortwin_venue_state::raydium_v4::read(&owner, &acc.data) {
                    Ok(info) => {
                        let sol_is_coin = info.sol_is_coin();
                        let token = if sol_is_coin == Some(true) {
                            info.pc_vault_mint
                        } else {
                            info.coin_vault_mint
                        };
                        (
                            vec![("status", info.status.to_string())],
                            sol_is_coin.map(|s| (Pubkey::new_from_array(token), s)),
                        )
                    }
                    Err(_) => continue,
                }
            } else {
                match shortwin_venue_state::raydium_cpmm::read_pool(&owner, &acc.data) {
                    Ok(state) => {
                        let config = Pubkey::new_from_array(state.amm_config);
                        if !configs.contains(&config) {
                            configs.push(config);
                        }
                        let sol0 = state.sol_is_token0();
                        let token = if sol0 == Some(true) {
                            state.token_1_mint
                        } else {
                            state.token_0_mint
                        };
                        (
                            vec![("status", state.status.to_string())],
                            sol0.map(|s| (Pubkey::new_from_array(token), s)),
                        )
                    }
                    Err(_) => continue,
                }
            };
            if let Some((mint, sol_is_first)) = orientation {
                sqlx::query(
                    "INSERT INTO venue_pools (pool, venue, program, mint, market, sol_is_first, first_seen_slot) \
                     VALUES ($1, $2, $3, $4, (SELECT market FROM markets WHERE mint = $4), $5, $6) \
                     ON CONFLICT (pool) DO UPDATE SET sol_is_first = EXCLUDED.sol_is_first, \
                       market = COALESCE(EXCLUDED.market, venue_pools.market), updated_at = now()",
                )
                .bind(key.to_string())
                .bind(crate::model::venue_name(u8::try_from(*venue).unwrap_or(0)))
                .bind(acc.owner.to_string())
                .bind(mint.to_string())
                .bind(sol_is_first)
                .bind(slot_i64(slot))
                .execute(pool)
                .await?;
            }
            let venue = crate::model::venue_name(u8::try_from(*venue).unwrap_or(0));
            for (field, value) in fields {
                observe_admin_value(pool, &mut self.watched, key, field, slot, venue, value)
                    .await?;
            }
        }
        // The CPMM pools' `AmmConfig` rates (the writer reads the protocol and fund rates back
        // for the swaps' pricing reserves, `venue::CpmmFeeRates`).
        if configs.is_empty() {
            return Ok(());
        }
        let res = self
            .client
            .get_multiple_accounts_with_commitment(
                &configs,
                solana_commitment_config::CommitmentConfig::confirmed(),
            )
            .await?;
        let slot = res.context.slot;
        for (key, acc) in configs.iter().zip(res.value) {
            let Some(acc) = acc else { continue };
            let Ok(config) =
                shortwin_venue_state::raydium_cpmm::read_config(&acc.owner.to_bytes(), &acc.data)
            else {
                continue;
            };
            for (field, value) in cpmm_config_fields(&config) {
                observe_admin_value(
                    pool,
                    &mut self.watched,
                    key,
                    field,
                    slot,
                    "raydium_cpmm",
                    value,
                )
                .await?;
            }
        }
        Ok(())
    }
}

/// The watched rates of a CPMM `AmmConfig` (ppm): `trade_fee_rate`, `protocol_fee_rate`,
/// `fund_fee_rate`, `creator_fee_rate`.
pub fn cpmm_config_fields(c: &shortwin_venue_state::raydium_cpmm::AmmConfig) -> WatchedFields {
    vec![
        ("trade_fee_rate", c.trade_fee_rate.to_string()),
        (
            venue::CPMM_PROTOCOL_FEE_RATE,
            c.protocol_fee_rate.to_string(),
        ),
        (venue::CPMM_FUND_FEE_RATE, c.fund_fee_rate.to_string()),
        ("creator_fee_rate", c.creator_fee_rate.to_string()),
    ]
}

/// `token_metadata` of `mint` (the first observation wins): decimals and, when the Metaplex
/// metadata account parsed, its name / symbol / uri, length-capped (symbol 16, name 64, uri 200;
/// §10.3).
pub async fn insert_token_metadata(
    pool: &PgPool,
    mint: &Pubkey,
    decimals: u8,
    metaplex: Option<(String, String, String)>,
    slot: u64,
) -> anyhow::Result<()> {
    let (source, name, symbol, uri) = match metaplex {
        Some((n, s, u)) => ("metaplex", Some(n), Some(s), Some(u)),
        None => ("none", None, None, None),
    };
    sqlx::query(
        "INSERT INTO token_metadata (mint, decimals, symbol, name, uri, source, fetched_slot) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (mint) DO NOTHING",
    )
    .bind(mint.to_string())
    .bind(i16::from(decimals))
    .bind(symbol.map(|s| cap(&s, 16)))
    .bind(name.map(|s| cap(&s, 64)))
    .bind(uri.map(|s| cap(&s, 200)))
    .bind(source)
    .bind(slot_i64(slot))
    .execute(pool)
    .await
    .context("token_metadata")?;
    Ok(())
}

/// The venue admin watch's last value of each watched `(account, field)`.
pub type AdminValues = HashMap<(Pubkey, &'static str), String>;

/// One observation of a watched venue admin field: a row only when `value` differs from the last
/// one. The last value is `known`'s, else (the first observation since the indexer started) the
/// latest stored row's, so a restart neither repeats unchanged values nor loses the old value of
/// one that changed while the indexer was down. Returns whether a row was recorded.
pub async fn observe_admin_value(
    pool: &PgPool,
    known: &mut AdminValues,
    account: &Pubkey,
    field: &'static str,
    slot: u64,
    venue: &str,
    value: String,
) -> anyhow::Result<bool> {
    let prev = match known.insert((*account, field), value.clone()) {
        Some(prev) => Some(prev),
        None => sqlx::query_scalar(
            "SELECT new_value FROM venue_admin_states WHERE account = $1 AND field = $2 \
             ORDER BY observed_slot DESC LIMIT 1",
        )
        .bind(account.to_string())
        .bind(field)
        .fetch_optional(pool)
        .await
        .context("venue_admin_states")?,
    };
    if prev.as_deref() == Some(value.as_str()) {
        return Ok(false);
    }
    record_admin_value(pool, account, field, slot, venue, prev.as_deref(), &value).await?;
    Ok(true)
}

/// One observed change of a watched venue admin field (`venue_admin_states`, §7.3).
pub async fn record_admin_value(
    pool: &PgPool,
    account: &Pubkey,
    field: &str,
    slot: u64,
    venue: &str,
    old: Option<&str>,
    new: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO venue_admin_states (account, field, observed_slot, venue, old_value, new_value) \
         VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
    )
    .bind(account.to_string())
    .bind(field)
    .bind(slot_i64(slot))
    .bind(venue)
    .bind(old)
    .bind(new)
    .execute(pool)
    .await
    .context("venue_admin_states")?;
    Ok(())
}

/// The watched fields of one venue account: `(field, value)`.
pub type WatchedFields = Vec<(&'static str, String)>;

/// Display text from untrusted token metadata (M9-SEC-5): every control character (NUL padding
/// included) and every invisible formatting character — bidi embeddings, overrides and isolates,
/// zero-width characters, the BOM, variation selectors, tag characters — is dropped, whitespace
/// runs collapse to one space, and the result is trimmed and capped at `n` characters. The web app
/// normalises again (NFC, `apps/web/src/lib/displayText.ts`) and shows the mint next to a symbol.
fn cap(s: &str, n: usize) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if invisible(c) {
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(c);
    }
    out.chars()
        .take(n)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// Control and invisible formatting characters (the same set the web app strips).
fn invisible(c: char) -> bool {
    c.is_control()
        || matches!(
            u32::from(c),
            0xAD | 0x61C
                | 0x115F
                | 0x1160
                | 0x17B4
                | 0x17B5
                | 0x180B..=0x180F
                | 0x200B..=0x200F
                | 0x2028..=0x202E
                | 0x2060..=0x206F
                | 0x3164
                | 0xFE00..=0xFE0F
                | 0xFEFF
                | 0xFFA0
                | 0xFFF9..=0xFFFB
                | 0xE0000..=0xE007F
        )
}

/// Metaplex `Metadata`: key (1), update_authority (32), mint (32), then Borsh strings name,
/// symbol, uri.
fn metaplex_strings(d: &[u8]) -> Option<(String, String, String)> {
    let mut o = 65usize;
    let mut s = || -> Option<String> {
        let n = usize::try_from(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?)).ok()?;
        let b = d.get(o + 4..o + 4 + n)?;
        o += 4 + n;
        Some(String::from_utf8_lossy(b).into_owned())
    };
    Some((s()?, s()?, s()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metaplex_names_are_read_and_capped() {
        let mut d = vec![4u8];
        d.extend([0u8; 64]);
        for s in ["Name\0\0", "SYM", "https://x"] {
            d.extend(u32::try_from(s.len()).expect("len").to_le_bytes());
            d.extend(s.as_bytes());
        }
        let (n, s, u) = metaplex_strings(&d).expect("parsed");
        assert_eq!(cap(&n, 64), "Name");
        // M9-SEC-5: bidi overrides, zero-width and control characters never reach the API.
        assert_eq!(cap("USDC\u{202E}\u{200B}\u{7}", 16), "USDC");
        assert_eq!(
            cap("  Wrapped \u{2066}SOL\u{2069}\t\n coin ", 64),
            "Wrapped SOL coin"
        );
        assert_eq!(cap("ABCDEFGHIJKLMNOPQRS", 16), "ABCDEFGHIJKLMNOP");
        assert_eq!(s, "SYM");
        assert_eq!(u, "https://x");
        assert!(metaplex_strings(&d[..70]).is_none());
    }

    /// The watched CPMM rates are the config's, under the names the writer reads back; every
    /// dumped mainnet `AmmConfig` holds the decoder's default protocol and fund rates.
    #[test]
    fn cpmm_config_rates_are_watched_under_the_writers_names() {
        use shortwin_venue_state::raydium_cpmm::{PROGRAM, read_config};
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/accounts/raydium"
        );
        for i in 0..=20 {
            let path = format!("{dir}/cpmm_config_{i:02}.bin");
            let data = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
            let c = read_config(&PROGRAM, &data).expect("an AmmConfig");
            assert_eq!(c.index, i);
            let f: HashMap<_, _> = cpmm_config_fields(&c).into_iter().collect();
            assert_eq!(f["trade_fee_rate"], c.trade_fee_rate.to_string());
            assert_eq!(f["creator_fee_rate"], c.creator_fee_rate.to_string());
            let rates = venue::CpmmFeeRates {
                protocol: f[venue::CPMM_PROTOCOL_FEE_RATE].parse().expect("u64"),
                fund: f[venue::CPMM_FUND_FEE_RATE].parse().expect("u64"),
            };
            assert_eq!(rates, venue::CpmmFeeRates::MAINNET, "{path}");
        }
    }
}
