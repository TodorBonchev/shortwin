//! DB-vs-chain assertions (INDEXER.md §11.4). Owner: ANALYTICS + REPLAY.
//!
//! Each function compares one part of an indexed database with the chain — a checkpoint's
//! accounts ([`ChainState`]) — or with the harness's independent decoding of the session's
//! events ([`Model`]), and returns every mismatch as a line of text (empty: equal):
//!
//! | §11.4 | Function |
//! |---|---|
//! | 1 markets, 2 vaults, 3 positions, 4 lenders and share epochs, 5 admin | [`projections_vs_chain`] |
//! | 1 markets' `vest_bal` ([E\*]) against the model at every checkpoint | [`vest_vs_model`] |
//! | 6 ledger identities, fees by type | [`ledgers`] |
//! | 7 analytics (views vs `formulas` on the accounts; settlements, exits, lender loss) | [`analytics`] |
//! | 7 rollups reproduce the views' end state | [`rollups`] |
//! | 8 idempotence, 9 rebuild, 10 out of order, 11 rollback | [`dump`] / [`diff_dumps`] in [`replay_session`] |
//! | 12 catalog coverage, nothing quarantined | [`catalog_gaps`], [`quarantined`] |
//! | the API's read-model assumptions (§9.2) | [`read_model`] |
//!
//! [`replay_session`] runs the whole list on one session through the INGEST pipeline
//! (`shortwin_indexer::ingest::ingest_session`, `ingest::rebuild`, and for item 11 the finality
//! tracker `ingest::finality::run_once` with a stub cluster, then the rollups again).

use std::collections::{BTreeMap, BTreeSet};

use anchor_lang::prelude::Pubkey;
use anyhow::Context;
use shortwin_indexer::analytics::{formulas, rollups as rollup_job};
use shortwin_indexer::ingest::finality::{self, BlockCheck, StatusSource, TxStatus};
use shortwin_indexer::ingest::raw::{Commitment, RawTx};
use shortwin_indexer::ingest::session::Session;
use shortwin_indexer::ingest::writer::Writer;
use shortwin_indexer::ingest::{self, IngestReport};
use shortwin_indexer::testing::pg::TestPg;
use solana_signature::Signature;
use sqlx::{PgPool, Row};

use crate::chain::{self, ChainState, Model};

/// A `v_market_exits` row: market, positions, open, closes, CR and expired liquidations,
/// expiries, S1, S2, S3.
type ExitRow = (String, i64, i64, i64, i64, i64, i64, i64, i64, i64);

/// `Pubkey::default()` → `None` (the schema's NULL), else base58.
pub fn key_or_null(k: &Pubkey) -> Option<String> {
    (*k != Pubkey::default()).then(|| k.to_string())
}

struct Diff(Vec<String>);

impl Diff {
    fn eq(&mut self, what: impl FnOnce() -> String, db: impl ToString, chain: impl ToString) {
        let (d, c) = (db.to_string(), chain.to_string());
        if d != c {
            self.0.push(format!("{}: db {d} ≠ chain {c}", what()));
        }
    }
}

fn opt<T: ToString>(v: Option<T>) -> String {
    v.map_or_else(|| "NULL".to_string(), |x| x.to_string())
}

/// Shares carried to the market's current epoch through the chain's `ShareEpoch` records.
fn chain_shares_now(c: &ChainState, market: &Pubkey, shares: u128, from: u32, to: u32) -> u128 {
    let recs = (from..to).filter_map(|ep| {
        c.share_epochs.get(&(*market, ep)).map(|r| {
            (
                r.num,
                1000u128.checked_pow(u32::from(r.den_log1000)).unwrap_or(0),
            )
        })
    });
    formulas::shares_at_epoch(shares, recs)
}

// ---- 1–5: projections against the accounts ----------------------------------------------------

/// §11.4 items 1–5: every [E] column of INDEXER.md §6 against the checkpoint's accounts.
#[allow(clippy::too_many_lines)]
pub async fn projections_vs_chain(pool: &PgPool, c: &ChainState) -> anyhow::Result<Vec<String>> {
    let mut d = Diff(Vec::new());
    // 1, 2: markets and their vaults.
    let rows = sqlx::query(
        "SELECT market, tokens_free::text, borrowed_q::text, borrowed_q_tier0::text, \
                borrowed_q_tier1::text, borrowed_q_tier2::text, borrowed_q_tier3::text, \
                total_shares::text, share_epoch, open_positions, total_notional_open::text, \
                max_q_hwm::text, venue, paused_opens, paused_coverage, \
                COALESCE(coverage_cleared_at, 0) AS coverage_cleared_at, mint_unsafe, \
                route_broken, wipe_live, ray_pool, COALESCE(pool_d, 0)::text AS pool_d, \
                fp_candidate, open_vault_lamports::text, lender_sol::text, protocol_fees::text, \
                lender_count, vest_bal::text \
         FROM markets",
    )
    .fetch_all(pool)
    .await
    .context("markets")?;
    let mut seen = BTreeSet::new();
    for r in &rows {
        let key: String = r.try_get("market")?;
        let mk: Pubkey = key.parse().map_err(|e| anyhow::anyhow!("{e:?}"))?;
        seen.insert(mk);
        let Some(m) = c.markets.get(&mk) else {
            d.0.push(format!("market {key}: in the database, not on chain"));
            continue;
        };
        let s = |w: &str| format!("market {key} {w}");
        let flag = |b: u16| m.flags & b != 0;
        use shortwin::state as st;
        d.eq(
            || s("tokens_free"),
            r.try_get::<String, _>(1)?,
            m.tokens_free,
        );
        d.eq(|| s("borrowed_q"), r.try_get::<String, _>(2)?, m.borrowed_q);
        for t in 0..4 {
            d.eq(
                || s(&format!("borrowed_q_tier{t}")),
                r.try_get::<String, _>(3 + t)?,
                m.borrowed_q_by_tier[t],
            );
        }
        d.eq(
            || s("total_shares"),
            r.try_get::<String, _>(7)?,
            m.total_shares.get(),
        );
        d.eq(|| s("share_epoch"), r.try_get::<i64, _>(8)?, m.share_epoch);
        d.eq(
            || s("open_positions"),
            r.try_get::<i64, _>(9)?,
            m.open_positions,
        );
        d.eq(
            || s("total_notional_open"),
            r.try_get::<String, _>(10)?,
            m.total_notional_open,
        );
        d.eq(|| s("max_q_hwm"), r.try_get::<String, _>(11)?, m.max_q_hwm);
        d.eq(|| s("venue"), r.try_get::<i16, _>(12)?, m.venue);
        d.eq(
            || s("paused_opens"),
            r.try_get::<bool, _>(13)?,
            flag(st::MARKET_FLAG_PAUSED_OPENS),
        );
        d.eq(
            || s("paused_coverage"),
            r.try_get::<bool, _>(14)?,
            flag(st::MARKET_FLAG_PAUSED_COVERAGE),
        );
        d.eq(
            || s("coverage_cleared_at"),
            r.try_get::<i64, _>(15)?,
            m.coverage_cleared_at,
        );
        d.eq(
            || s("mint_unsafe"),
            r.try_get::<bool, _>(16)?,
            flag(st::MARKET_FLAG_MINT_UNSAFE),
        );
        d.eq(
            || s("route_broken"),
            r.try_get::<bool, _>(17)?,
            flag(st::MARKET_FLAG_ROUTE_BROKEN),
        );
        d.eq(
            || s("wipe_live"),
            r.try_get::<bool, _>(18)?,
            flag(st::MARKET_FLAG_WIPE_LIVE),
        );
        if m.venue == 4 || m.venue == 5 {
            d.eq(
                || s("ray_pool"),
                opt(r.try_get::<Option<String>, _>(19)?),
                opt(key_or_null(&m.pool_cache[st::raydium_cache::RAY_POOL])),
            );
        }
        // `pool_d` (column 20) is [E*], not [E]: every observation's prologue refreshes it
        // from the incumbent's locked depth (`engine/prologue.rs`); not compared.
        d.eq(
            || s("fp_candidate"),
            opt(r.try_get::<Option<String>, _>(21)?),
            opt(key_or_null(&m.fp_keys[0])),
        );
        let open_v: u64 = c.positions_of(&mk).map(|(_, p)| p.v).sum();
        d.eq(
            || s("open_vault_lamports (Σ open v)"),
            r.try_get::<String, _>(22)?,
            open_v,
        );
        match c.vault_of(&mk) {
            Some(v) => {
                d.eq(
                    || s("lender_sol"),
                    r.try_get::<String, _>(23)?,
                    v.lender_sol,
                );
                d.eq(
                    || s("protocol_fees"),
                    r.try_get::<String, _>(24)?,
                    v.protocol_fees,
                );
            }
            None => d.0.push(s("no MarketVault on chain")),
        }
        let lenders = c
            .lenders
            .iter()
            .filter(|((mm, _), l)| {
                *mm == mk && chain_shares_now(c, &mk, l.shares, l.epoch, m.share_epoch) > 0
            })
            .count();
        d.eq(|| s("lender_count"), r.try_get::<i64, _>(25)?, lenders);
        // `vest_bal` is [E*], exact where a flush left none (AIR-07): a vest at 0 before its end
        // was flushed with nothing vested since, and every flush is reported
        // (`SolSettle.vest_flushed`, `EpochRolled`, a lender event with `s_after = 0`).
        if m.vest_bal == 0 && m.vest_end > m.vest_last {
            d.eq(
                || s("vest_bal (flushed)"),
                opt(r.try_get::<Option<String>, _>(26)?),
                0,
            );
        }
    }
    for k in c.markets.keys() {
        if !seen.contains(k) {
            d.0.push(format!("market {k}: on chain, not in the database"));
        }
    }

    // 3: positions.
    let rows = sqlx::query(
        "SELECT position, status, q::text, s::text, m::text, margin_added::text, v::text, \
                n_open::text, fee_open::text, deadline, tier, armed, \
                COALESCE(s_count, 0)::text AS s_count, COALESCE(s_since, 0) AS s_since, \
                rent_protocol_bps, penalty_lender_bps, penalty_protocol_bps, \
                penalty_keeper_bps, expiry_fee_bps \
         FROM positions",
    )
    .fetch_all(pool)
    .await
    .context("positions")?;
    let mut in_db = BTreeSet::new();
    for r in &rows {
        let key: String = r.try_get(0)?;
        let pk: Pubkey = key.parse().map_err(|e| anyhow::anyhow!("{e:?}"))?;
        in_db.insert(pk);
        let status: String = r.try_get(1)?;
        let s = |w: &str| format!("position {key} {w}");
        let Some(p) = c.positions.get(&pk) else {
            if status == "open" {
                d.0.push(s("open in the database, closed on chain"));
            }
            continue;
        };
        d.eq(|| s("status"), &status, "open");
        d.eq(|| s("q"), r.try_get::<String, _>(2)?, p.q);
        d.eq(|| s("s"), r.try_get::<String, _>(3)?, p.s);
        let m: u64 = r.try_get::<String, _>(4)?.parse()?;
        let added: u64 = r.try_get::<String, _>(5)?.parse()?;
        d.eq(|| s("m + margin_added"), m + added, p.m);
        d.eq(|| s("v"), r.try_get::<String, _>(6)?, p.v);
        d.eq(|| s("n_open"), r.try_get::<String, _>(7)?, p.n_open);
        d.eq(|| s("fee_open"), r.try_get::<String, _>(8)?, p.fee_open);
        d.eq(|| s("deadline"), r.try_get::<i64, _>(9)?, p.deadline);
        d.eq(|| s("tier"), r.try_get::<i16, _>(10)?, p.tier);
        d.eq(|| s("armed"), r.try_get::<bool, _>(11)?, p.s_armed != 0);
        if p.s_armed != 0 {
            d.eq(|| s("s_count"), r.try_get::<String, _>(12)?, p.s_count);
            d.eq(|| s("s_since"), r.try_get::<i64, _>(13)?, p.s_since);
        }
        let terms = [
            ("rent_protocol_bps", p.rent_protocol_bps, 14),
            ("penalty_lender_bps", p.penalty_split[0], 15),
            ("penalty_protocol_bps", p.penalty_split[1], 16),
            ("penalty_keeper_bps", p.penalty_split[2], 17),
            ("expiry_fee_bps", p.expiry_fee_bps, 18),
        ];
        for (name, want, i) in terms {
            d.eq(|| s(name), opt(r.try_get::<Option<i32>, _>(i)?), want);
        }
    }
    for k in c.positions.keys() {
        if !in_db.contains(k) {
            d.0.push(format!("position {k}: open on chain, not in the database"));
        }
    }

    // 4: lenders and share epochs.
    let rows = sqlx::query(
        "SELECT market, owner, shares::text, epoch, lock::text, lock_until FROM lender_positions",
    )
    .fetch_all(pool)
    .await
    .context("lender_positions")?;
    let mut lenders_db = BTreeSet::new();
    for r in &rows {
        let (mk, owner): (String, String) = (r.try_get(0)?, r.try_get(1)?);
        let mkk: Pubkey = mk.parse().map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let ok: Pubkey = owner.parse().map_err(|e| anyhow::anyhow!("{e:?}"))?;
        lenders_db.insert((mkk, ok));
        let s = |w: &str| format!("lender {owner}@{mk} {w}");
        let shares: String = r.try_get(2)?;
        let lock: String = r.try_get(4)?;
        match c.lenders.get(&(mkk, ok)) {
            Some(l) => {
                d.eq(|| s("shares"), &shares, l.shares);
                d.eq(|| s("epoch"), r.try_get::<i64, _>(3)?, l.epoch);
                d.eq(|| s("lock"), &lock, l.lock);
                d.eq(|| s("lock_until"), r.try_get::<i64, _>(5)?, l.lock_until);
            }
            None => {
                let pda = shortwin_litesvm_tests::pda::lender(&mkk, &ok).0;
                if c.closed.contains(&pda) {
                    d.eq(|| s("shares (closed account)"), &shares, 0);
                    d.eq(|| s("lock (closed account)"), &lock, 0);
                } else {
                    d.0.push(s("in the database, no LenderPosition on chain"));
                }
            }
        }
    }
    for k in c.lenders.keys() {
        if !lenders_db.contains(k) {
            d.0.push(format!(
                "lender {}@{}: on chain, not in the database",
                k.1, k.0
            ));
        }
    }
    let rows = sqlx::query("SELECT market, epoch, num, den::text FROM share_epochs")
        .fetch_all(pool)
        .await
        .context("share_epochs")?;
    let mut epochs_db = BTreeSet::new();
    for r in &rows {
        let mk: String = r.try_get(0)?;
        let mkk: Pubkey = mk.parse().map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let epoch = u32::try_from(r.try_get::<i64, _>(1)?)?;
        epochs_db.insert((mkk, epoch));
        let s = || format!("share epoch {mk}#{epoch}");
        match c.share_epochs.get(&(mkk, epoch)) {
            Some(rec) => {
                let den = 1000u128
                    .checked_pow(u32::from(rec.den_log1000))
                    .unwrap_or(0);
                d.eq(
                    s,
                    format!("{}/{}", r.try_get::<i16, _>(2)?, r.try_get::<String, _>(3)?),
                    format!("{}/{den}", rec.num),
                );
            }
            None => d.0.push(format!("{}: no ShareEpoch record on chain", s())),
        }
    }
    for k in c.share_epochs.keys() {
        if !epochs_db.contains(k) {
            d.0.push(format!(
                "share epoch {}#{}: on chain, not in the database",
                k.0, k.1
            ));
        }
    }

    // 5: admin.
    admin(pool, c, &mut d).await?;
    Ok(d.0)
}

async fn admin(pool: &PgPool, c: &ChainState, d: &mut Diff) -> anyhow::Result<()> {
    let Some(cfg) = c.config.as_ref() else {
        return Ok(());
    };
    let row = sqlx::query(
        "SELECT governance, pause_authority, coverage_authority, fee_recipient, timelock_s, \
                util_kink_bps, pyth_sol_usd, pyth_feed_id, \
                params->>'rent_protocol_bps', params->>'penalty_lender_bps', \
                params->>'penalty_protocol_bps', params->>'penalty_keeper_bps', \
                params->>'expiry_fee_bps', params->>'open_fee_bps', \
                params->>'market_creation_fee' \
         FROM protocol_params_history ORDER BY slot DESC, tx_index DESC, ix_index DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .context("protocol_params_history")?;
    let Some(r) = row else {
        d.0.push("protocol_params_history: empty, GlobalConfig on chain".into());
        return Ok(());
    };
    let s = |w: &str| format!("config {w}");
    d.eq(
        || s("governance"),
        r.try_get::<String, _>(0)?,
        cfg.governance,
    );
    d.eq(
        || s("pause_authority"),
        r.try_get::<String, _>(1)?,
        cfg.pause_authority,
    );
    d.eq(
        || s("coverage_authority"),
        opt(r.try_get::<Option<String>, _>(2)?),
        opt(key_or_null(&cfg.coverage_authority)),
    );
    d.eq(
        || s("fee_recipient"),
        r.try_get::<String, _>(3)?,
        cfg.fee_recipient,
    );
    d.eq(|| s("timelock_s"), r.try_get::<i64, _>(4)?, cfg.timelock_s);
    d.eq(
        || s("util_kink_bps"),
        r.try_get::<i64, _>(5)?,
        cfg.kink.util_kink_bps,
    );
    d.eq(
        || s("pyth_sol_usd"),
        r.try_get::<String, _>(6)?,
        cfg.pyth_sol_usd,
    );
    d.eq(
        || s("pyth_feed_id"),
        r.try_get::<String, _>(7)?,
        shortwin_indexer::numeric::hex(&cfg.pyth_feed_id),
    );
    let p = &cfg.params;
    let fields: [(&str, String); 7] = [
        ("rent_protocol_bps", p.rent_protocol_bps.to_string()),
        ("penalty_lender_bps", p.penalty_lender_bps.to_string()),
        ("penalty_protocol_bps", p.penalty_protocol_bps.to_string()),
        ("penalty_keeper_bps", p.penalty_keeper_bps.to_string()),
        ("expiry_fee_bps", p.expiry_fee_bps.to_string()),
        ("open_fee_bps", p.open_fee_bps.to_string()),
        ("market_creation_fee", p.market_creation_fee.to_string()),
    ];
    for (i, (name, want)) in fields.iter().enumerate() {
        d.eq(
            || s(&format!("params.{name}")),
            opt(r.try_get::<Option<String>, _>(8 + i)?),
            want,
        );
    }
    let (paused, broken, enabled): (bool, i64, Option<i64>) =
        sqlx::query_as("SELECT paused_opens, route_broken_bits, venue_enabled FROM protocol_state")
            .fetch_one(pool)
            .await?;
    d.eq(
        || s("paused_opens (protocol_state)"),
        paused,
        cfg.paused_opens(),
    );
    d.eq(
        || s("route_broken_bits (protocol_state)"),
        broken,
        cfg.venue_route_broken,
    );
    d.eq(
        || s("venue_enabled (protocol_state)"),
        opt(enabled),
        cfg.venue_enabled,
    );
    if let Some(g) = c.guard.as_ref() {
        let rows: Vec<(i16, bool, String)> =
            sqlx::query_as("SELECT venue, upgraded, guard_slot::text FROM venue_guards")
                .fetch_all(pool)
                .await?;
        let mut seen = BTreeSet::new();
        for (v, upgraded, slot) in rows {
            let i = usize::try_from(v)?;
            seen.insert(i);
            d.eq(
                || s(&format!("venue_guards[{v}].upgraded")),
                upgraded,
                g.upgraded & (1 << v) != 0,
            );
            d.eq(
                || s(&format!("venue_guards[{v}].guard_slot")),
                slot,
                g.guard_slot.get(i).copied().unwrap_or_default(),
            );
        }
        for v in 0..6usize {
            if g.upgraded & (1 << v) != 0 && !seen.contains(&v) {
                d.0.push(s(&format!("venue_guards[{v}]: flagged on chain, no row")));
            }
        }
    }
    let rows: Vec<(i16, i16, i64, Vec<String>, i64)> =
        sqlx::query_as("SELECT venue, ix, version, keys, stable_version FROM venue_extras")
            .fetch_all(pool)
            .await?;
    for (venue, ix, version, keys, stable) in rows {
        let key = shortwin_litesvm_tests::pda::venue_extras_seed(u8::try_from(venue)?);
        let Some(x) = c.extras.get(&key) else {
            d.0.push(s(&format!(
                "venue_extras[{venue}/{ix}]: no VenueExtras on chain"
            )));
            continue;
        };
        let Some(set) = x.sets.get(usize::try_from(ix)?) else {
            d.0.push(s(&format!("venue_extras[{venue}/{ix}]: no such set")));
            continue;
        };
        d.eq(
            || s(&format!("venue_extras[{venue}/{ix}].version")),
            version,
            set.current.version,
        );
        d.eq(
            || s(&format!("venue_extras[{venue}/{ix}].stable_version")),
            stable,
            set.stable.version,
        );
        let chain_keys: Vec<String> = set.current.entries[..usize::from(set.current.count).min(4)]
            .iter()
            .map(|e| e.key.to_string())
            .collect();
        d.eq(
            || s(&format!("venue_extras[{venue}/{ix}].keys")),
            keys.iter()
                .take(chain_keys.len())
                .cloned()
                .collect::<Vec<_>>()
                .join(","),
            chain_keys.join(","),
        );
    }
    Ok(())
}

/// `markets.vest_bal` ([E\*], INDEXER.md §6.2) against the harness's model of the same events
/// (`MarketModel::vest_bal`: the last `PriceCranked` snapshot, 0 after every reported flush, a
/// flushing rebase's record donation, AIR-07): INGEST's projection and the contract's rule,
/// applied independently, agree for every market; the model is held to the chain wherever the
/// rule is exact (`Model::check_against`).
pub async fn vest_vs_model(pool: &PgPool, model: &Model) -> anyhow::Result<Vec<String>> {
    let mut d = Diff(Vec::new());
    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT market, vest_bal::text FROM markets")
            .fetch_all(pool)
            .await
            .context("markets.vest_bal")?;
    for (market, vest) in rows {
        let mk: Pubkey = market.parse().map_err(|e| anyhow::anyhow!("{e:?}"))?;
        if let Some(m) = model.markets.get(&mk) {
            d.eq(
                || format!("market {market} vest_bal vs the model"),
                opt(vest),
                opt(m.vest_bal),
            );
        }
    }
    Ok(d.0)
}

// ---- 6: ledgers -----------------------------------------------------------------------------------

/// §11.4 item 6: per market, `protocol_fees = Σ fees(protocol) − Σ protocol_sweeps` and
/// `lender_sol = Σ lender inflows − Σ sol_paid` inside the database — the record donations of
/// §6.4 included, from `EpochRolled`'s `events.derived.record_donation` (IX-11, IX-13), each
/// market's total also checked against the model's System transfers —; and the fee ledger by
/// `(market, fee_type)` against the events (the model's legs).
pub async fn ledgers(pool: &PgPool, model: &Model) -> anyhow::Result<Vec<String>> {
    let mut d = Diff(Vec::new());
    let rows: Vec<(String, String, String, String, String, String)> = sqlx::query_as(
        "SELECT m.market, m.protocol_fees::text, \
                (COALESCE((SELECT sum(amount) FROM fees f \
                           WHERE f.market = m.market AND f.recipient = 'protocol'), 0) \
                 - COALESCE((SELECT sum(amount) FROM protocol_sweeps s \
                             WHERE s.market = m.market), 0))::text, \
                m.lender_sol::text, \
                (COALESCE((SELECT sum(amount) FROM fees f \
                           WHERE f.market = m.market AND f.recipient = 'lenders'), 0) \
                 + COALESCE((SELECT sum(lender_amount) FROM settlements s \
                             WHERE s.market = m.market), 0) \
                 + COALESCE((SELECT sum((data->>'amount')::numeric) FROM events e \
                             WHERE e.market = m.market AND e.kind = 'DeficitCovered'), 0) \
                 + COALESCE((SELECT sum((derived->>'record_donation')::numeric) FROM events e \
                             WHERE e.market = m.market AND e.kind = 'EpochRolled'), 0) \
                 - COALESCE((SELECT sum((data->>'sol_paid')::numeric) FROM events e \
                             WHERE e.market = m.market AND e.kind IN ('Deposit', 'Withdraw', \
                                   'ClaimSol', 'DeficitCovered', 'LenderSynced')), 0))::text, \
                COALESCE((SELECT sum((derived->>'record_donation')::numeric) FROM events e \
                          WHERE e.market = m.market AND e.kind = 'EpochRolled'), 0)::text \
         FROM markets m",
    )
    .fetch_all(pool)
    .await
    .context("ledger identities")?;
    for (market, pf, pf_sum, ls, ls_sum, donated) in rows {
        let mk: Pubkey = market.parse().map_err(|e| anyhow::anyhow!("{e:?}"))?;
        d.eq(
            || format!("market {market} record donations (events.derived) vs System transfers"),
            donated,
            model.donations.get(&mk).copied().unwrap_or(0),
        );
        d.eq(
            || format!("market {market} protocol_fees vs Σ protocol fees − sweeps"),
            pf,
            pf_sum,
        );
        d.eq(
            || format!("market {market} lender_sol vs Σ lender inflows − sol_paid"),
            ls,
            ls_sum,
        );
    }
    let mut want: BTreeMap<(String, String), (u64, u128)> = BTreeMap::new();
    for f in &model.fees {
        let e = want
            .entry((f.market.to_string(), f.fee_type.to_string()))
            .or_default();
        e.0 += 1;
        e.1 += u128::from(f.amount);
    }
    let rows: Vec<(String, String, i64, String)> = sqlx::query_as(
        "SELECT market, fee_type, count(*), sum(amount)::text FROM fees GROUP BY 1, 2",
    )
    .fetch_all(pool)
    .await?;
    let mut got: BTreeMap<(String, String), (u64, u128)> = BTreeMap::new();
    for (m, t, nrows, sum) in rows {
        got.insert((m, t), (u64::try_from(nrows)?, sum.parse()?));
    }
    for k in want.keys().chain(got.keys()).collect::<BTreeSet<_>>() {
        d.eq(
            || format!("fees {}/{} (legs, Σ amount)", k.0, k.1),
            format!("{:?}", got.get(k)),
            format!("{:?}", want.get(k)),
        );
    }
    Ok(d.0)
}

// ---- 7: analytics ---------------------------------------------------------------------------------

/// §11.4 item 7: `v_market_overview`'s TVL, OI and utilisation against `formulas` on the
/// accounts (with the projection's `P_ema`); every settlement's realised-lender-loss inputs and
/// outcome, the exit counts, the lender-loss and global aggregates against the model.
#[allow(clippy::too_many_lines)]
pub async fn analytics(
    pool: &PgPool,
    c: &ChainState,
    model: &Model,
) -> anyhow::Result<Vec<String>> {
    let mut d = Diff(Vec::new());
    let rows: Vec<(String, String, String, i64, Option<String>)> = sqlx::query_as(
        "SELECT market, tvl_lamports::text, oi_lamports::text, util_bps, ema_price::text \
         FROM v_market_overview",
    )
    .fetch_all(pool)
    .await
    .context("v_market_overview")?;
    for (market, tvl, oi, util, ema) in rows {
        let mk: Pubkey = market.parse().map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let (Some(m), Some(v)) = (c.markets.get(&mk), c.vault_of(&mk)) else {
            continue;
        };
        let ema: u128 = ema.map_or(Ok(0), |e| e.parse())?;
        let open_v: u64 = c.positions_of(&mk).map(|(_, p)| p.v).sum();
        let s = |w: &str| format!("v_market_overview {market} {w}");
        d.eq(
            || s("tvl_lamports"),
            tvl,
            formulas::tvl_lamports(m.tokens_free, ema, v.lender_sol, open_v),
        );
        d.eq(
            || s("oi_lamports"),
            oi,
            formulas::oi_lamports(m.borrowed_q, ema),
        );
        d.eq(
            || s("util_bps"),
            util,
            shortwin_indexer::numeric::util_bps(m.tokens_free, m.borrowed_q),
        );
    }
    // Settlements, one by one.
    let rows = sqlx::query(
        "SELECT position, condition, q::text, n_open::text, lender_amount::text, p_ref::text, \
                p_ref_source, token_value::text, lender_pnl::text, realised_lender_loss::text, \
                p_lo::text, p_hi::text \
         FROM settlements",
    )
    .fetch_all(pool)
    .await
    .context("settlements")?;
    let by_pos: BTreeMap<String, &chain::SettlementModel> = model
        .settlements
        .iter()
        .map(|s| (s.position.to_string(), s))
        .collect();
    let mut seen = BTreeSet::new();
    for r in &rows {
        let pos: String = r.try_get(0)?;
        seen.insert(pos.clone());
        let Some(m) = by_pos.get(&pos) else {
            d.0.push(format!(
                "settlement {pos}: in the database, not in the events"
            ));
            continue;
        };
        let s = |w: &str| format!("settlement {pos} {w}");
        d.eq(|| s("condition"), r.try_get::<i16, _>(1)?, m.condition);
        d.eq(|| s("q"), r.try_get::<String, _>(2)?, m.q);
        d.eq(|| s("n_open"), r.try_get::<String, _>(3)?, m.n_open);
        d.eq(
            || s("lender_amount"),
            r.try_get::<String, _>(4)?,
            m.lender_amount,
        );
        d.eq(|| s("p_lo"), r.try_get::<String, _>(10)?, m.p_lo);
        d.eq(|| s("p_hi"), r.try_get::<String, _>(11)?, m.p_hi);
        d.eq(|| s("p_ref"), r.try_get::<String, _>(5)?, m.p_ref);
        d.eq(
            || s("p_ref_source"),
            r.try_get::<String, _>(6)?,
            m.p_ref_source,
        );
        // The outcome from the row's own inputs (formulas::settlement_outcome) and the model's.
        let (amount, q, p_ref): (u64, u64, u128) = (
            r.try_get::<String, _>(4)?.parse()?,
            r.try_get::<String, _>(2)?.parse()?,
            r.try_get::<String, _>(5)?.parse()?,
        );
        let o = formulas::settlement_outcome(amount, q, p_ref);
        d.eq(
            || s("token_value"),
            r.try_get::<String, _>(7)?,
            o.token_value,
        );
        d.eq(|| s("lender_pnl"), r.try_get::<String, _>(8)?, o.lender_pnl);
        d.eq(
            || s("realised_lender_loss"),
            r.try_get::<String, _>(9)?,
            o.realised_lender_loss,
        );
        d.eq(
            || s("realised_lender_loss (model)"),
            o.realised_lender_loss,
            m.realised_lender_loss,
        );
    }
    for p in by_pos.keys() {
        if !seen.contains(p) {
            d.0.push(format!(
                "settlement {p}: in the events, not in the database"
            ));
        }
    }
    // Exits.
    let rows: Vec<ExitRow> = sqlx::query_as(
        "SELECT market, positions, open, closes, liquidations_cr, liquidations_expired, expiries, \
                settlements_s1, settlements_s2, settlements_s3 FROM v_market_exits",
    )
    .fetch_all(pool)
    .await?;
    for r in rows {
        let mk: Pubkey = r.0.parse().map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let e = model.exits(&mk);
        d.eq(
            || format!("v_market_exits {}", r.0),
            format!("{:?}", (r.1, r.2, r.3, r.4, r.5, r.6, r.7, r.8, r.9)),
            format!(
                "{:?}",
                (
                    e.positions,
                    e.open,
                    e.closes,
                    e.liquidations_cr,
                    e.liquidations_expired,
                    e.expiries,
                    e.settlements_s1,
                    e.settlements_s2,
                    e.settlements_s3
                )
            ),
        );
    }
    // Lender loss by market.
    let rows: Vec<(String, i64, String, String, Option<String>)> = sqlx::query_as(
        "SELECT market, settlements, realised_lender_loss::text, lender_pnl::text, \
                loss_bps_of_value::text FROM v_market_lender_loss",
    )
    .fetch_all(pool)
    .await?;
    for (market, nset, loss, pnl, bps) in rows {
        let ss: Vec<_> = model
            .settlements
            .iter()
            .filter(|s| s.market.to_string() == market)
            .collect();
        let want_loss: u128 = ss.iter().map(|s| s.realised_lender_loss).sum();
        let want_pnl: i128 = ss.iter().map(|s| s.lender_pnl).sum();
        let value: u128 = ss.iter().map(|s| s.token_value).sum();
        let s = |w: &str| format!("v_market_lender_loss {market} {w}");
        d.eq(|| s("settlements"), nset, ss.len());
        d.eq(|| s("realised_lender_loss"), loss, want_loss);
        d.eq(|| s("lender_pnl"), pnl, want_pnl);
        d.eq(
            || s("loss_bps_of_value"),
            opt(bps),
            opt(formulas::share_bps(want_loss, value)),
        );
    }
    // Global totals.
    let g = sqlx::query(
        "SELECT protocol_revenue::text, lender_fee_income::text, keeper_income::text, closes, \
                liquidations, expiries, settlements_s1, settlements_s2, settlements_s3, \
                realised_lender_loss::text, settlement_lender_pnl::text, coverage_gap_s::text \
         FROM v_global_stats",
    )
    .fetch_one(pool)
    .await
    .context("v_global_stats")?;
    let sum_by = |rec: &str| -> u128 {
        model
            .fees
            .iter()
            .filter(|f| f.recipient == rec)
            .map(|f| u128::from(f.amount))
            .sum()
    };
    let count = |st: &str| model.positions.values().filter(|p| p.status == st).count();
    let s_count = |c: u8| {
        model
            .settlements
            .iter()
            .filter(|s| s.condition == c)
            .count()
    };
    let s = |w: &str| format!("v_global_stats {w}");
    d.eq(
        || s("protocol_revenue"),
        g.try_get::<String, _>(0)?,
        sum_by("protocol"),
    );
    d.eq(
        || s("lender_fee_income"),
        g.try_get::<String, _>(1)?,
        sum_by("lenders"),
    );
    d.eq(
        || s("keeper_income"),
        g.try_get::<String, _>(2)?,
        sum_by("keeper"),
    );
    d.eq(|| s("closes"), g.try_get::<i64, _>(3)?, count("closed"));
    d.eq(
        || s("liquidations"),
        g.try_get::<i64, _>(4)?,
        count("liquidated"),
    );
    d.eq(|| s("expiries"), g.try_get::<i64, _>(5)?, count("expired"));
    d.eq(|| s("settlements_s1"), g.try_get::<i64, _>(6)?, s_count(1));
    d.eq(|| s("settlements_s2"), g.try_get::<i64, _>(7)?, s_count(2));
    d.eq(|| s("settlements_s3"), g.try_get::<i64, _>(8)?, s_count(3));
    d.eq(
        || s("realised_lender_loss"),
        g.try_get::<String, _>(9)?,
        model
            .settlements
            .iter()
            .map(|x| x.realised_lender_loss)
            .sum::<u128>(),
    );
    d.eq(
        || s("settlement_lender_pnl"),
        g.try_get::<String, _>(10)?,
        model.settlements.iter().map(|x| x.lender_pnl).sum::<i128>(),
    );
    d.eq(
        || s("coverage_gap_s"),
        g.try_get::<String, _>(11)?,
        model.coverage_gaps.iter().map(|(_, g)| *g).sum::<i64>(),
    );
    Ok(d.0)
}

/// §11.4 item 7 (rollups): one pass over the whole session (`rollups::pass` up to the hour
/// after the newest event); each market's last hour holds the market's current state and the
/// views' TVL / OI at that hour's `P_ema`, and the hours' flows sum to the session's totals.
#[allow(clippy::too_many_lines)]
pub async fn rollups(pool: &PgPool, model: &Model) -> anyhow::Result<Vec<String>> {
    let mut d = Diff(Vec::new());
    let end: Option<i64> = sqlx::query_scalar("SELECT max(ev_ts) FROM events")
        .fetch_one(pool)
        .await?;
    let Some(end) = end else {
        return Ok(Vec::new());
    };
    let until = rollup_job::bucket_of(end) + rollup_job::BUCKET_S;
    rollup_job::pass(pool, Some(until)).await?;
    let rows = sqlx::query(
        "SELECT DISTINCT ON (h.market) h.market, h.tokens_free::text, h.borrowed_q::text, \
                h.total_shares::text, h.open_positions, h.total_notional_open::text, \
                h.open_vault_lamports::text, h.lender_sol::text, h.ema_price::text, \
                h.oi_lamports::text, h.tvl_lamports::text, \
                m.tokens_free::text, m.borrowed_q::text, m.total_shares::text, m.open_positions, \
                m.total_notional_open::text, m.open_vault_lamports::text, m.lender_sol::text \
         FROM market_stats_hourly h JOIN markets m ON m.market = h.market \
         ORDER BY h.market, h.bucket_start DESC",
    )
    .fetch_all(pool)
    .await
    .context("market_stats_hourly")?;
    for r in &rows {
        let market: String = r.try_get(0)?;
        let s = |w: &str| format!("rollups {market} last hour {w}");
        let names = [
            "tokens_free",
            "borrowed_q",
            "total_shares",
            "open_positions",
            "total_notional_open",
            "open_vault_lamports",
            "lender_sol",
        ];
        for (i, name) in names.iter().enumerate() {
            let (h, m) = if i == 3 {
                (
                    r.try_get::<i64, _>(1 + i)?.to_string(),
                    r.try_get::<i64, _>(11 + i)?.to_string(),
                )
            } else {
                (
                    r.try_get::<String, _>(1 + i)?,
                    r.try_get::<String, _>(11 + i)?,
                )
            };
            d.eq(|| s(name), h, m);
        }
        let ema: u128 = r
            .try_get::<Option<String>, _>(8)?
            .map_or(Ok(0), |e| e.parse())?;
        let t: u64 = r.try_get::<String, _>(1)?.parse()?;
        let b: u64 = r.try_get::<String, _>(2)?.parse()?;
        let ls: u64 = r.try_get::<String, _>(7)?.parse()?;
        let v: u64 = r.try_get::<String, _>(6)?.parse()?;
        d.eq(
            || s("oi_lamports"),
            r.try_get::<String, _>(9)?,
            formulas::oi_lamports(b, ema),
        );
        d.eq(
            || s("tvl_lamports"),
            r.try_get::<String, _>(10)?,
            formulas::tvl_lamports(t, ema, ls, v),
        );
    }
    let g = sqlx::query(
        "SELECT COALESCE(sum(fees_protocol), 0)::text, COALESCE(sum(fees_lenders), 0)::text, \
                COALESCE(sum(fees_keeper), 0)::text, COALESCE(sum(opens), 0)::bigint, \
                COALESCE(sum(closes), 0)::bigint, COALESCE(sum(liquidations), 0)::bigint, \
                COALESCE(sum(expiries), 0)::bigint, COALESCE(sum(settlements_s1), 0)::bigint, \
                COALESCE(sum(settlements_s2), 0)::bigint, COALESCE(sum(settlements_s3), 0)::bigint, \
                COALESCE(sum(realised_lender_loss), 0)::text, COALESCE(sum(coverage_gaps), 0)::bigint \
         FROM market_stats_hourly",
    )
    .fetch_one(pool)
    .await?;
    let sum_by = |rec: &str| -> u128 {
        model
            .fees
            .iter()
            .filter(|f| f.recipient == rec)
            .map(|f| u128::from(f.amount))
            .sum()
    };
    let count = |st: &str| model.positions.values().filter(|p| p.status == st).count();
    let s_count = |c: u8| {
        model
            .settlements
            .iter()
            .filter(|s| s.condition == c)
            .count()
    };
    let s = |w: &str| format!("rollups Σ hours {w}");
    d.eq(
        || s("fees_protocol"),
        g.try_get::<String, _>(0)?,
        sum_by("protocol"),
    );
    d.eq(
        || s("fees_lenders"),
        g.try_get::<String, _>(1)?,
        sum_by("lenders"),
    );
    d.eq(
        || s("fees_keeper"),
        g.try_get::<String, _>(2)?,
        sum_by("keeper"),
    );
    d.eq(
        || s("opens"),
        g.try_get::<i64, _>(3)?,
        model.positions.len(),
    );
    d.eq(|| s("closes"), g.try_get::<i64, _>(4)?, count("closed"));
    d.eq(
        || s("liquidations"),
        g.try_get::<i64, _>(5)?,
        count("liquidated"),
    );
    d.eq(|| s("expiries"), g.try_get::<i64, _>(6)?, count("expired"));
    d.eq(|| s("settlements_s1"), g.try_get::<i64, _>(7)?, s_count(1));
    d.eq(|| s("settlements_s2"), g.try_get::<i64, _>(8)?, s_count(2));
    d.eq(|| s("settlements_s3"), g.try_get::<i64, _>(9)?, s_count(3));
    d.eq(
        || s("realised_lender_loss"),
        g.try_get::<String, _>(10)?,
        model
            .settlements
            .iter()
            .map(|x| x.realised_lender_loss)
            .sum::<u128>(),
    );
    d.eq(
        || s("coverage_gaps"),
        g.try_get::<i64, _>(11)?,
        model.coverage_gaps.len(),
    );
    // The global hours are the markets' sums.
    let bad: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM global_stats_hourly g \
         JOIN (SELECT bucket_start, count(*) AS markets, sum(tvl_lamports) AS tvl, \
                      sum(fees_protocol) AS fp, sum(opens) AS opens \
               FROM market_stats_hourly GROUP BY bucket_start) h USING (bucket_start) \
         WHERE g.markets <> h.markets OR g.tvl_lamports <> h.tvl OR g.fees_protocol <> h.fp \
            OR g.opens <> h.opens",
    )
    .fetch_one(pool)
    .await?;
    d.eq(
        || "global_stats_hourly hours that are not the markets' sums".into(),
        bad,
        0,
    );
    Ok(d.0)
}

// ---- the API's read model ----------------------------------------------------------------------

/// What the API's queries assume of INGEST's rows (INDEXER.md §9.2; the API track's list):
/// - every exit event (`Liquidate`, `Expire`, `SolSettle`) has `events.actor` = its keeper and
///   `events.position` = its position, and a `positions` row exists for that position (exits
///   join it);
/// - every position event has `events.position` = its position;
/// - every lender event has `events.actor` = its owner (`/v1/lenders/{owner}/events`, the
///   `lender:` channel);
/// - every cancelled `config_changes` row names who cancelled it;
/// - every venue trade with both amounts non-zero has its price (candles).
pub async fn read_model(pool: &PgPool) -> anyhow::Result<Vec<String>> {
    let checks: [(&str, &str); 6] = [
        (
            "exit events: actor = keeper, position = the event's",
            "SELECT signature || ' ' || kind FROM events \
             WHERE kind IN ('Liquidate', 'Expire', 'SolSettle') \
               AND (actor IS DISTINCT FROM (data->>'keeper') \
                    OR position IS DISTINCT FROM (data->>'position'))",
        ),
        (
            "exit events: a positions row",
            "SELECT e.signature || ' ' || e.kind FROM events e \
             WHERE e.kind IN ('Liquidate', 'Expire', 'SolSettle') \
               AND NOT EXISTS (SELECT 1 FROM positions p WHERE p.position = e.position)",
        ),
        (
            "position events: position = the event's",
            "SELECT signature || ' ' || kind FROM events \
             WHERE kind IN ('Open', 'Close', 'MarginAdded', 'SettleArmed', 'SettleDisarmed') \
               AND position IS DISTINCT FROM (data->>'position')",
        ),
        (
            "lender events: actor = owner",
            "SELECT signature || ' ' || kind FROM events \
             WHERE kind IN ('Deposit', 'Withdraw', 'ClaimSol', 'DeficitCovered', 'LenderSynced') \
               AND actor IS DISTINCT FROM (data->>'owner')",
        ),
        (
            "cancelled config changes: cancelled_by",
            "SELECT proposed_signature::text FROM config_changes \
             WHERE status = 'cancelled' AND cancelled_by IS NULL",
        ),
        (
            "venue trades: price_wad",
            "SELECT signature::text FROM venue_trades \
             WHERE token_amount > 0 AND sol_amount > 0 AND price_wad IS NULL",
        ),
    ];
    let mut out = Vec::new();
    for (what, sql) in checks {
        let bad: Vec<String> = sqlx::query_scalar(sql)
            .fetch_all(pool)
            .await
            .with_context(|| format!("read model: {what}"))?;
        if !bad.is_empty() {
            out.push(format!(
                "read model ({what}): {} rows, e.g. {:?}",
                bad.len(),
                &bad[..bad.len().min(3)]
            ));
        }
    }
    Ok(out)
}

// ---- 8–12 -----------------------------------------------------------------------------------------

/// The projection and decoded-row tables [`dump`] compares (INDEXER.md §4.3), with the columns
/// that legitimately differ between equal databases left out (ingestion times and sequence).
pub const DUMP_TABLES: [&str; 20] = [
    "ingest_txs",
    "events",
    "invocations",
    "markets",
    "market_state_log",
    "positions",
    "lender_positions",
    "share_epochs",
    "settlements",
    "fees",
    "protocol_sweeps",
    "market_snapshots",
    "coverage_events",
    "pool_events",
    "pools",
    "config_changes",
    "protocol_params_history",
    "protocol_state",
    "venue_guards",
    "venue_extras",
];

/// Every row of [`DUMP_TABLES`] as canonical JSON text (sorted), without `id`, `seq`,
/// `ingested_at`, `updated_at`, `computed_at`, `fetched_at`, `observed_at`.
pub async fn dump(pool: &PgPool) -> anyhow::Result<BTreeMap<String, Vec<String>>> {
    let mut out = BTreeMap::new();
    for t in DUMP_TABLES {
        let sql = format!(
            "SELECT (to_jsonb(x) - ARRAY['id', 'seq', 'ingested_at', 'updated_at', 'computed_at', \
             'fetched_at', 'observed_at'])::text FROM {t} x ORDER BY 1"
        );
        let rows: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .fetch_all(pool)
            .await
            .with_context(|| format!("dumping {t}"))?;
        out.insert(t.to_string(), rows);
    }
    Ok(out)
}

/// The differences of two dumps (first few rows per table).
pub fn diff_dumps(
    what: &str,
    a: &BTreeMap<String, Vec<String>>,
    b: &BTreeMap<String, Vec<String>>,
) -> Vec<String> {
    let mut out = Vec::new();
    for (t, ra) in a {
        let rb = b.get(t).cloned().unwrap_or_default();
        if *ra == rb {
            continue;
        }
        let sa: BTreeSet<&String> = ra.iter().collect();
        let sb: BTreeSet<&String> = rb.iter().collect();
        let only_a: Vec<_> = sa.difference(&sb).take(3).collect();
        let only_b: Vec<_> = sb.difference(&sa).take(3).collect();
        out.push(format!(
            "{what}: table {t} differs ({} vs {} rows); only in the first: {only_a:?}; only in \
             the second: {only_b:?}",
            ra.len(),
            rb.len()
        ));
    }
    out
}

/// Rows in `ingest_quarantine` (§11.4 item 12: none).
pub async fn quarantined(pool: &PgPool) -> anyhow::Result<Vec<String>> {
    let rows: Vec<(String, i16, i16, i32, String)> = sqlx::query_as(
        "SELECT signature, ix_index, ev_source, ev_seq, reason FROM ingest_quarantine",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            format!(
                "quarantined {} ix {} src {} seq {}: {}",
                r.0, r.1, r.2, r.3, r.4
            )
        })
        .collect())
}

/// Event kinds of the catalog (`model::SHORTWIN_EVENTS`) no model of `models` saw, except those
/// in `never` (never emitted by the program as built, or not reachable in the session set — each
/// with its reason).
pub fn catalog_gaps(models: &[&Model], never: &[(&str, &str)]) -> Vec<String> {
    let seen: BTreeSet<&str> = models
        .iter()
        .flat_map(|m| m.kinds.keys().copied())
        .collect();
    shortwin_indexer::model::SHORTWIN_EVENTS
        .iter()
        .map(|s| s.name)
        .filter(|n| !seen.contains(n) && !never.iter().any(|(k, _)| k == n))
        .map(|n| format!("event kind {n}: in no session"))
        .collect()
}

/// What [`replay_session`] checked.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// Checkpoints whose projections were compared with the chain.
    pub checkpoints: usize,
    /// The in-order ingest of the whole session.
    pub ingest: IngestReport,
    /// Every mismatch.
    pub failures: Vec<String>,
}

/// Options of [`replay_session`].
#[derive(Clone, Copy, Debug)]
pub struct ReplayOptions {
    /// Compare the projections at every `stride`-th checkpoint (1: all; the last one always).
    pub stride: usize,
    /// Transactions the rollback check removes from the end (§11.4 item 11).
    pub rollback_last: usize,
}

impl Default for ReplayOptions {
    fn default() -> Self {
        Self {
            stride: 1,
            rollback_last: 5,
        }
    }
}

/// The cluster as the finality tracker sees it in item 11: every transaction up to `finalized`
/// is finalized in its slot except `abandoned`, which no node knows, and whose slots' finalized
/// blocks do not carry them (the fork was abandoned).
struct AbandonedFork {
    finalized: u64,
    abandoned: BTreeSet<String>,
    slots: BTreeMap<String, u64>,
}

impl StatusSource for AbandonedFork {
    async fn finalized_slot(&self) -> anyhow::Result<u64> {
        Ok(self.finalized)
    }

    async fn statuses(&self, sigs: &[Signature]) -> anyhow::Result<Vec<Option<TxStatus>>> {
        Ok(sigs
            .iter()
            .map(|s| {
                let s = s.to_string();
                (!self.abandoned.contains(&s)).then(|| TxStatus {
                    slot: self.slots.get(&s).copied().unwrap_or(self.finalized),
                    finalized: true,
                })
            })
            .collect())
    }

    async fn fetch_finalized(&self, _sig: &Signature) -> anyhow::Result<Option<RawTx>> {
        Ok(None)
    }

    async fn block_check(&self, _slot: u64, sig: &Signature) -> anyhow::Result<BlockCheck> {
        Ok(if self.abandoned.contains(&sig.to_string()) {
            BlockCheck::Lacks
        } else {
            BlockCheck::Has
        })
    }
}

/// `market_stats_hourly` and `global_stats_hourly` as canonical JSON text (sorted), without
/// `computed_at` / `updated_at`.
pub async fn dump_rollups(pool: &PgPool) -> anyhow::Result<BTreeMap<String, Vec<String>>> {
    let mut out = BTreeMap::new();
    for t in ["market_stats_hourly", "global_stats_hourly"] {
        let sql = format!(
            "SELECT (to_jsonb(x) - ARRAY['computed_at', 'updated_at'])::text FROM {t} x ORDER BY 1"
        );
        let rows: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .fetch_all(pool)
            .await
            .with_context(|| format!("dumping {t}"))?;
        out.insert(t.to_string(), rows);
    }
    Ok(out)
}

/// The whole of INDEXER.md §11.4 on `session` through the INGEST pipeline, on fresh databases
/// of `pg`.
#[allow(clippy::too_many_lines)]
pub async fn replay_session(
    pg: &TestPg,
    session: &Session,
    opts: ReplayOptions,
) -> anyhow::Result<Report> {
    let txs = chain::decode_session(session)?;
    let mut model = Model::new(session)?;
    let mut report = Report::default();
    let pool = pg.fresh_db().await?;
    // 1–5 at the checkpoints, 6–7 at the end.
    let mut next = 0usize;
    let n_cp = session.checkpoints.len();
    for (i, (k, c)) in session.checkpoints.iter().enumerate() {
        if *k > next {
            let r = ingest::ingest_session(&pool, session, next..*k).await?;
            report.ingest.txs_seen += r.txs_seen;
            report.ingest.txs_written += r.txs_written;
            report.ingest.events += r.events;
            model.apply_all(&txs[next..*k]);
            next = *k;
        }
        if i % opts.stride.max(1) != 0 && i + 1 != n_cp {
            continue;
        }
        let state = ChainState::from_checkpoint(c)?;
        report.checkpoints += 1;
        for f in projections_vs_chain(&pool, &state).await? {
            report
                .failures
                .push(format!("checkpoint after tx {k}: {f}"));
        }
        for f in vest_vs_model(&pool, &model).await? {
            report
                .failures
                .push(format!("checkpoint after tx {k}: {f}"));
        }
    }
    if next < session.txs.len() {
        let r = ingest::ingest_session(&pool, session, next..session.txs.len()).await?;
        report.ingest.txs_written += r.txs_written;
        model.apply_all(&txs[next..]);
    }
    let end = session
        .end_state()
        .map(ChainState::from_checkpoint)
        .transpose()?
        .unwrap_or_default();
    report.failures.extend(ledgers(&pool, &model).await?);
    report
        .failures
        .extend(analytics(&pool, &end, &model).await?);
    report.failures.extend(quarantined(&pool).await?);
    report.failures.extend(read_model(&pool).await?);
    let base = dump(&pool).await?;
    // 8: idempotence.
    let again = ingest::ingest_session(&pool, session, 0..session.txs.len()).await?;
    if again.txs_written != 0 {
        report.failures.push(format!(
            "re-ingest wrote {} transactions",
            again.txs_written
        ));
    }
    report
        .failures
        .extend(diff_dumps("re-ingest", &base, &dump(&pool).await?));
    // 9: rebuild.
    ingest::rebuild(&pool, None).await?;
    report
        .failures
        .extend(diff_dumps("rebuild", &base, &dump(&pool).await?));
    // 7 (rollups) after the dumps (the rollup tables are not dumped).
    report.failures.extend(rollups(&pool, &model).await?);
    // 10: out of order — the second half first (a gap-fill), then the first, then a rebuild.
    let ooo = pg.fresh_db().await?;
    let half = session.txs.len() / 2;
    ingest::ingest_session(&ooo, session, half..session.txs.len()).await?;
    ingest::ingest_session(&ooo, session, 0..half).await?;
    ingest::rebuild(&ooo, None).await?;
    report
        .failures
        .extend(diff_dumps("out of order", &base, &dump(&ooo).await?));
    ooo.close().await;
    // 11: rollback of the last k transactions through the finality tracker (§3.2, §3.4): the
    // session ingested at `confirmed` and rolled up; the cluster then finalizes the first n − k
    // and knows nothing of the last k (an abandoned fork). The database must equal one that only
    // ever saw the first n − k (finalized) — the rollups too, recomputed from the hour the
    // rollback moved the rollup clock back to (`rollups::reset_from`).
    let k = opts.rollback_last.min(session.txs.len());
    if k > 0 {
        let cut = session.txs.len() - k;
        let last_ts = txs.iter().map(|t| t.block_time).max().unwrap_or(0);
        let until = rollup_job::bucket_of(last_ts) + rollup_job::BUCKET_S;
        let rolled = pg.fresh_db().await?;
        ingest::replay::ingest_session_at(
            &rolled,
            session,
            0..session.txs.len(),
            Commitment::Confirmed,
        )
        .await?;
        rollup_job::run_once(&rolled, Some(until)).await?;
        let cluster = AbandonedFork {
            finalized: session.txs.iter().map(|t| t.slot).max().unwrap_or(0),
            abandoned: txs[cut..].iter().map(|t| t.signature.clone()).collect(),
            slots: txs.iter().map(|t| (t.signature.clone(), t.slot)).collect(),
        };
        let mut writer =
            Writer::open(&rolled, ingest::replay::program_ids(session)?, "replay").await?;
        let fr = finality::run_once(&mut writer, &cluster, 256).await;
        writer.close().await?;
        let fr = fr?;
        // The refresher's part of §3.4: the rollback dropped the account reads at or after the
        // abandoned slot (the [A] columns they fed, e.g. `markets.coin_creator`); the refresher
        // reads the accounts again — the chain as of the surviving transactions, i.e. the
        // checkpoints up to the cut.
        let program = ingest::replay::program_ids(session)?.shortwin;
        for (at, cp) in &session.checkpoints {
            if *at <= cut {
                ingest::refresher::apply_checkpoint(&rolled, cp, &program).await?;
            }
        }
        if fr.rolled_back != k as u64 || fr.promoted != cut as u64 {
            report.failures.push(format!(
                "finality: rolled back {} (want {k}), promoted {} (want {cut})",
                fr.rolled_back, fr.promoted
            ));
        }
        rollup_job::run_once(&rolled, Some(until)).await?;
        let prefix = pg.fresh_db().await?;
        ingest::ingest_session(&prefix, session, 0..cut).await?;
        rollup_job::run_once(&prefix, Some(until)).await?;
        // `ingest_rollbacks` (the audit row) and `ingest_state` are not dumped.
        report.failures.extend(diff_dumps(
            "rollback",
            &dump(&prefix).await?,
            &dump(&rolled).await?,
        ));
        report.failures.extend(diff_dumps(
            "rollback (rollups)",
            &dump_rollups(&prefix).await?,
            &dump_rollups(&rolled).await?,
        ));
        rolled.close().await;
        prefix.close().await;
    }
    pool.close().await;
    Ok(report)
}
