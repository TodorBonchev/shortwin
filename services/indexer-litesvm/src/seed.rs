//! TEST-ONLY reference writer (ANALYTICS + REPLAY): stores the harness's own [`Model`] of a
//! session — the contract's projection rules applied to the independently decoded events — in
//! the schema of `services/indexer/migrations`, exactly as INDEXER.md §6 and §8 describe the
//! rows.
//!
//! It is **not** the indexer's writer (that is INGEST's `ingest::writer`, reached through
//! `ingest::ingest_session`). The harness uses it to run its DB-side assertions
//! ([`crate::assert`]) and the analytics views and rollups on recorded sessions on their own:
//! the assertions' SQL is checked against the real schema, the views and rollups are checked on
//! realistic data, and a tampered row proves each assertion can fail. The INGEST pipeline is
//! then held to the same assertions (`assert::replay_session`).

use anchor_lang::prelude::Pubkey;
use anyhow::Context;
use shortwin::state::GlobalConfig;
use sqlx::{PgPool, Postgres, Transaction};

use crate::chain::{ChainState, ChainTx, Key, Model};

fn k(p: &Pubkey) -> String {
    p.to_string()
}

fn ko(p: Option<Pubkey>) -> Option<String> {
    p.map(|x| x.to_string())
}

fn n<T: ToString>(v: T) -> String {
    v.to_string()
}

fn i16_of(v: u16) -> i32 {
    i32::from(v)
}

fn small(v: u8) -> i16 {
    i16::from(v)
}

fn big(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Writes `txs` and the rows of `model` (which must have collected them: [`Model::collecting`]
/// applied to exactly `txs`). `state`, when given, supplies the configuration rows
/// (`protocol_params_history` with `source = 'account'`, `protocol_state`, `venue_guards`)
/// from its accounts.
pub async fn write(
    pool: &PgPool,
    txs: &[ChainTx],
    model: &Model,
    state: Option<&ChainState>,
) -> anyhow::Result<()> {
    let rows = model
        .rows
        .as_ref()
        .context("the model did not collect rows (Model::collecting)")?;
    let mut db = pool.begin().await?;
    for t in txs {
        tx_row(&mut db, t).await?;
    }
    for e in &rows.events {
        sqlx::query(
            "INSERT INTO events (signature, ix_index, ev_source, ev_seq, slot, tx_index, block_time, \
             program, invocation, kind, market, position, actor, ev_slot, ev_ts, data, derived) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)",
        )
        .bind(&e.key.signature)
        .bind(i16::try_from(e.key.ix_index)?)
        .bind(small(e.key.ev_source))
        .bind(i32::try_from(e.key.ev_seq)?)
        .bind(big(e.key.slot))
        .bind(i32::try_from(e.key.tx_index)?)
        .bind(e.block_time)
        .bind(k(&e.program))
        .bind(&e.invocation)
        .bind(e.kind)
        .bind(ko(e.market))
        .bind(ko(e.position))
        .bind(ko(e.actor))
        .bind(big(e.ev_slot))
        .bind(e.ev_ts)
        .bind(&e.data)
        .bind(&e.derived)
        .execute(&mut *db)
        .await
        .with_context(|| format!("events {:?}", e.key))?;
    }
    for m in model.markets.values() {
        market_row(&mut db, model, m).await?;
    }
    for l in &rows.logs {
        sqlx::query(
            "INSERT INTO market_state_log (signature, ix_index, ev_source, ev_seq, market, slot, \
             tx_index, ts, kind, tokens_free, borrowed_q, total_shares, open_positions, \
             total_notional_open, open_vault_lamports, lender_sol, protocol_fees, ema_price) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::numeric, $11::numeric, \
                     $12::numeric, $13, $14::numeric, $15::numeric, $16::numeric, $17::numeric, \
                     $18::numeric)",
        )
        .bind(&l.key.signature)
        .bind(i16::try_from(l.key.ix_index)?)
        .bind(small(l.key.ev_source))
        .bind(i32::try_from(l.key.ev_seq)?)
        .bind(k(&l.market))
        .bind(big(l.key.slot))
        .bind(i32::try_from(l.key.tx_index)?)
        .bind(l.ts)
        .bind(l.kind)
        .bind(n(l.tokens_free))
        .bind(n(l.borrowed_q))
        .bind(n(l.total_shares))
        .bind(i64::from(l.open_positions))
        .bind(n(l.total_notional_open))
        .bind(n(l.open_vault))
        .bind(n(l.lender_sol))
        .bind(n(l.protocol_fees))
        .bind(l.ema_price.map(n))
        .execute(&mut *db)
        .await
        .context("market_state_log")?;
    }
    for p in model.positions.values() {
        position_row(&mut db, p).await?;
    }
    for ((market, owner), l) in &model.lenders {
        sqlx::query(
            "INSERT INTO lender_positions (market, owner, shares, epoch, lock, lock_until, \
             deposited_tokens, withdrawn_tokens, sol_paid, pot_draw, deficit_covered, deposits, \
             withdrawals, claims, first_slot, first_ts, last_slot, last_ts) \
             VALUES ($1, $2, $3::numeric, $4, $5::numeric, $6, $7::numeric, $8::numeric, \
                     $9::numeric, $10::numeric, $11::numeric, $12, $13, $14, $15, $16, $17, $18)",
        )
        .bind(k(market))
        .bind(k(owner))
        .bind(n(l.shares))
        .bind(i64::from(l.epoch))
        .bind(n(l.lock))
        .bind(l.lock_until)
        .bind(n(l.deposited_tokens))
        .bind(n(l.withdrawn_tokens))
        .bind(n(l.sol_paid))
        .bind(n(l.pot_draw))
        .bind(n(l.deficit_covered))
        .bind(i64::from(l.deposits))
        .bind(i64::from(l.withdrawals))
        .bind(i64::from(l.claims))
        .bind(big(l.first_slot))
        .bind(l.first_ts)
        .bind(big(l.last_slot))
        .bind(l.last_ts)
        .execute(&mut *db)
        .await
        .context("lender_positions")?;
    }
    for r in &rows.epochs {
        sqlx::query(
            "INSERT INTO share_epochs (market, epoch, acc_end, num, den, vest_flushed, full_wipe, \
             signature, ix_index, ev_source, ev_seq, slot, ts) \
             VALUES ($1, $2, $3::numeric, $4, $5::numeric, $6::numeric, $7, $8, $9, $10, $11, \
                     $12, $13)",
        )
        .bind(k(&r.market))
        .bind(i64::from(r.epoch))
        .bind(n(r.acc_end))
        .bind(small(r.num))
        .bind(n(r.den))
        .bind(n(r.vest_flushed))
        .bind(r.num == 0)
        .bind(&r.key.signature)
        .bind(i16::try_from(r.key.ix_index)?)
        .bind(small(r.key.ev_source))
        .bind(i32::try_from(r.key.ev_seq)?)
        .bind(big(r.key.slot))
        .bind(r.ts)
        .execute(&mut *db)
        .await
        .context("share_epochs")?;
    }
    for s in &model.settlements {
        sqlx::query(
            "INSERT INTO settlements (position, market, keeper, condition, b_live, b_rs, b_ema, \
             p_lo, p_hi, l_lo, l_hi, x_lo, x_hi, pprime_lo, pprime_hi, fv, k_s, lender_amount, \
             lump, borrower_amount, ema_age_s, q, n_open, tier, expired, p_ref, p_ref_source, \
             p_ref_age_s, token_value, lender_pnl, realised_lender_loss, signature, ix_index, \
             ev_source, ev_seq, slot, tx_index, ts) \
             VALUES ($1, $2, $3, $4, $5::numeric, $6::numeric, $7::numeric, $8::numeric, \
                     $9::numeric, $10::numeric, $11::numeric, $12::numeric, $13::numeric, $14, $15, \
                     $16::numeric, $17::numeric, $18::numeric, $19::numeric, $20::numeric, $21, \
                     $22::numeric, $23::numeric, $24, $25, $26::numeric, $27, $28, $29::numeric, \
                     $30::numeric, $31::numeric, $32, $33, $34, $35, $36, $37, $38)",
        )
        .bind(k(&s.position))
        .bind(k(&s.market))
        .bind(k(&s.keeper))
        .bind(small(s.condition))
        .bind(n(s.b_live))
        .bind(n(s.b_rs))
        .bind(n(s.b_ema))
        .bind(n(s.p_lo))
        .bind(n(s.p_hi))
        .bind(n(s.l_lo))
        .bind(n(s.l_hi))
        .bind(n(s.x_lo))
        .bind(n(s.x_hi))
        .bind(i16_of(s.pprime_lo))
        .bind(i16_of(s.pprime_hi))
        .bind(n(s.fv))
        .bind(n(s.k_s))
        .bind(n(s.lender_amount))
        .bind(n(s.lump))
        .bind(n(s.borrower_amount))
        .bind(s.ema_age_s)
        .bind(n(s.q))
        .bind(n(s.n_open))
        .bind(small(s.tier))
        .bind(s.expired)
        .bind(n(s.p_ref))
        .bind(s.p_ref_source)
        .bind(s.p_ref_age_s)
        .bind(n(s.token_value))
        .bind(n(s.lender_pnl))
        .bind(n(s.realised_lender_loss))
        .bind(&s.key.signature)
        .bind(i16::try_from(s.key.ix_index)?)
        .bind(small(s.key.ev_source))
        .bind(i32::try_from(s.key.ev_seq)?)
        .bind(big(s.key.slot))
        .bind(i32::try_from(s.key.tx_index)?)
        .bind(s.ts)
        .execute(&mut *db)
        .await
        .context("settlements")?;
    }
    for f in &model.fees {
        sqlx::query(
            "INSERT INTO fees (signature, ix_index, ev_source, ev_seq, fee_type, recipient, \
             recipient_key, market, position, amount, derived, slot, tx_index, ts) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::numeric, $11, $12, $13, $14)",
        )
        .bind(&f.key.signature)
        .bind(i16::try_from(f.key.ix_index)?)
        .bind(small(f.key.ev_source))
        .bind(i32::try_from(f.key.ev_seq)?)
        .bind(f.fee_type)
        .bind(f.recipient)
        .bind(ko(f.recipient_key))
        .bind(k(&f.market))
        .bind(ko(f.position))
        .bind(n(f.amount))
        .bind(f.derived)
        .bind(big(f.key.slot))
        .bind(i32::try_from(f.key.tx_index)?)
        .bind(f.ts)
        .execute(&mut *db)
        .await
        .context("fees")?;
    }
    for s in &rows.snapshots {
        sqlx::query(
            "INSERT INTO market_snapshots (signature, ix_index, ev_source, ev_seq, market, slot, \
             tx_index, ts, spot_wad, ema_price, ema_liq, liq_slow, ema_count, lb_lo, lb_hi, tier, \
             util_bps, rent_basis0, rent_basis1, rent_basis2, vest_bal, cranker) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::numeric, $10::numeric, $11::numeric, \
                     $12::numeric, $13::numeric, $14::numeric, $15::numeric, $16, $17, \
                     $18::numeric, $19::numeric, $20::numeric, $21::numeric, $22)",
        )
        .bind(&s.key.signature)
        .bind(i16::try_from(s.key.ix_index)?)
        .bind(small(s.key.ev_source))
        .bind(i32::try_from(s.key.ev_seq)?)
        .bind(k(&s.market))
        .bind(big(s.key.slot))
        .bind(i32::try_from(s.key.tx_index)?)
        .bind(s.ts)
        .bind(n(s.spot_wad))
        .bind(n(s.ema_price))
        .bind(n(s.ema_liq))
        .bind(n(s.liq_slow))
        .bind(n(s.ema_count))
        .bind(n(s.lb_lo))
        .bind(n(s.lb_hi))
        .bind(small(s.tier))
        .bind(i64::from(s.util_bps))
        .bind(n(s.rent_basis[0]))
        .bind(n(s.rent_basis[1]))
        .bind(n(s.rent_basis[2]))
        .bind(n(s.vest_bal))
        .bind(k(&s.cranker))
        .execute(&mut *db)
        .await
        .context("market_snapshots")?;
    }
    for c in &rows.coverage {
        sqlx::query(
            "INSERT INTO coverage_events (signature, ix_index, ev_source, ev_seq, market, kind, \
             gap_s, open_positions, paused, reason, by_key, slot, tx_index, ts) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
        )
        .bind(&c.key.signature)
        .bind(i16::try_from(c.key.ix_index)?)
        .bind(small(c.key.ev_source))
        .bind(i32::try_from(c.key.ev_seq)?)
        .bind(k(&c.market))
        .bind(c.kind)
        .bind(c.gap_s)
        .bind(c.open_positions.map(i64::from))
        .bind(c.paused)
        .bind(c.reason.map(small))
        .bind(ko(c.by))
        .bind(big(c.key.slot))
        .bind(i32::try_from(c.key.tx_index)?)
        .bind(c.ts)
        .execute(&mut *db)
        .await
        .context("coverage_events")?;
    }
    for (key, market, ts, amount, recipient) in &rows.sweeps {
        sqlx::query(
            "INSERT INTO protocol_sweeps (signature, ix_index, ev_source, ev_seq, market, amount, \
             recipient, slot, ts) VALUES ($1, $2, $3, $4, $5, $6::numeric, $7, $8, $9)",
        )
        .bind(&key.signature)
        .bind(i16::try_from(key.ix_index)?)
        .bind(small(key.ev_source))
        .bind(i32::try_from(key.ev_seq)?)
        .bind(k(market))
        .bind(n(amount))
        .bind(k(recipient))
        .bind(big(key.slot))
        .bind(ts)
        .execute(&mut *db)
        .await
        .context("protocol_sweeps")?;
    }
    let last = txs.iter().map(|t| (t.slot, t.block_time)).max();
    if let Some((slot, time)) = last {
        sqlx::query(
            "UPDATE ingest_state SET confirmed_slot = $1, finalized_slot = $1, \
             latest_block_time = $2, genesis_slot = $3",
        )
        .bind(big(slot))
        .bind(time)
        .bind(txs.first().map(|t| big(t.slot)))
        .execute(&mut *db)
        .await?;
    }
    if let (Some(state), Some(first)) = (state, txs.first()) {
        config_rows(&mut db, state, first).await?;
    }
    db.commit().await?;
    Ok(())
}

async fn tx_row(db: &mut Transaction<'_, Postgres>, t: &ChainTx) -> anyhow::Result<()> {
    let (err_ix, err_code) = match &t.err {
        Some(v) => {
            let ix = v
                .get("InstructionError")
                .and_then(|e| e.get(0))
                .and_then(serde_json::Value::as_i64)
                .and_then(|i| i16::try_from(i).ok());
            let code = v
                .get("InstructionError")
                .and_then(|e| e.get(1))
                .and_then(|e| e.get("Custom"))
                .and_then(serde_json::Value::as_i64);
            (ix, code)
        }
        None => (None, None),
    };
    sqlx::query(
        "INSERT INTO ingest_txs (signature, slot, tx_index, block_time, commitment, source, \
         fee_payer, fee, compute_units, err, err_ix_index, err_code, logs_truncated, shortwin_ixs) \
         VALUES ($1, $2, $3, $4, 'finalized', 'replay', $5, $6::numeric, $7, $8, $9, $10, $11, \
                 $12)",
    )
    .bind(&t.signature)
    .bind(big(t.slot))
    .bind(i32::try_from(t.tx_index)?)
    .bind(t.block_time)
    .bind(k(&t.fee_payer))
    .bind(n(t.fee))
    .bind(t.compute_units.map(big))
    .bind(&t.err)
    .bind(err_ix)
    .bind(err_code)
    .bind(t.logs_truncated)
    .bind(i16::try_from(
        t.invocations.iter().filter(|i| i.stack_height == 1).count(),
    )?)
    .execute(&mut **db)
    .await
    .with_context(|| format!("ingest_txs {}", t.signature))?;
    for inv in &t.invocations {
        sqlx::query(
            "INSERT INTO invocations (signature, ix_index, inner_index, stack_height, slot, \
             tx_index, block_time, program, name, market, failed) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(&t.signature)
        .bind(i16::try_from(inv.ix_index)?)
        .bind(i16::try_from(inv.inner_index)?)
        .bind(i16::try_from(inv.stack_height)?)
        .bind(big(t.slot))
        .bind(i32::try_from(t.tx_index)?)
        .bind(t.block_time)
        .bind(shortwin::ID.to_string())
        .bind(&inv.name)
        .bind(ko(inv.market))
        .bind(!t.ok)
        .execute(&mut **db)
        .await
        .context("invocations")?;
    }
    Ok(())
}

async fn market_row(
    db: &mut Transaction<'_, Postgres>,
    model: &Model,
    m: &crate::chain::MarketModel,
) -> anyhow::Result<()> {
    let last: &Key = &m.last;
    sqlx::query(
        "INSERT INTO markets (market, mint, token_program, creator, created_slot, created_ts, \
         created_signature, creation_fee, venue_at_listing, tier_at_listing, bonding_curve, \
         canonical_pool, venue, ray_pool, tier, paused_opens, paused_coverage, \
         coverage_cleared_at, mint_unsafe, route_broken, wipe_live, tokens_free, borrowed_q, \
         borrowed_q_tier0, borrowed_q_tier1, borrowed_q_tier2, borrowed_q_tier3, total_shares, \
         share_epoch, open_positions, total_notional_open, max_q_hwm, open_vault_lamports, \
         lender_sol, protocol_fees, lender_count, spot_wad, ema_price, price_ts, price_source, \
         fp_candidate, fp_venue, pool_d, last_slot, last_tx_index, last_ix_index, \
         last_ev_source, last_ev_seq, vest_bal) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8::numeric, $9, $10, $11, $12, $13, $14, $15, $16, \
                 $17, $18, $19, $20, $21, $22::numeric, $23::numeric, $24::numeric, $25::numeric, \
                 $26::numeric, $27::numeric, $28::numeric, $29, $30, $31::numeric, $32::numeric, \
                 $33::numeric, $34::numeric, $35::numeric, $36, $37::numeric, $38::numeric, $39, \
                 'event', $40, $41, $42::numeric, $43, $44, $45, $46, $47, $48::numeric)",
    )
    .bind(k(&m.market))
    .bind(k(&m.mint))
    .bind(k(&m.token_program))
    .bind(k(&m.creator))
    .bind(big(m.created_slot))
    .bind(m.created_ts)
    .bind(&m.created_signature)
    .bind(n(m.creation_fee))
    .bind(small(m.venue_at_listing))
    .bind(small(m.tier_at_listing))
    .bind(ko(m.bonding_curve))
    .bind(ko(m.canonical_pool))
    .bind(small(m.venue))
    .bind(ko(m.ray_pool))
    .bind(small(m.tier))
    .bind(m.paused_opens)
    .bind(m.paused_coverage)
    .bind((m.coverage_cleared_at != 0).then_some(m.coverage_cleared_at))
    .bind(m.mint_unsafe)
    .bind(m.route_broken)
    .bind(m.wipe_live)
    .bind(n(m.tokens_free))
    .bind(n(m.borrowed_q))
    .bind(n(m.borrowed_q_by_tier[0]))
    .bind(n(m.borrowed_q_by_tier[1]))
    .bind(n(m.borrowed_q_by_tier[2]))
    .bind(n(m.borrowed_q_by_tier[3]))
    .bind(n(m.total_shares))
    .bind(i64::from(m.share_epoch))
    .bind(i64::from(m.open_positions))
    .bind(n(m.total_notional_open))
    .bind(n(m.max_q_hwm))
    .bind(n(m.open_vault))
    .bind(n(m.lender_sol))
    .bind(n(m.protocol_fees))
    .bind(i64::from(model.lender_count(&m.market)))
    .bind(m.spot_wad.map(n))
    .bind(m.ema_price.map(n))
    .bind(m.ema_ts)
    .bind(ko(m.fp_candidate))
    .bind(m.fp_venue.map(small))
    .bind(n(m.pool_d))
    .bind(big(last.slot))
    .bind(i32::try_from(last.tx_index)?)
    .bind(i16::try_from(last.ix_index)?)
    .bind(small(last.ev_source))
    .bind(i32::try_from(last.ev_seq)?)
    .bind(m.vest_bal.map(n))
    .execute(&mut **db)
    .await
    .with_context(|| format!("markets {}", m.market))?;
    Ok(())
}

async fn position_row(
    db: &mut Transaction<'_, Postgres>,
    p: &crate::chain::PositionModel,
) -> anyhow::Result<()> {
    let t = p.terms;
    sqlx::query(
        "INSERT INTO positions (position, market, borrower, nonce, tier, venue, q, s, m, v_open, \
         margin_added, v, n_open, fee_open, venue_rent, entry_price, spot_wad_open, ema_wad_open, \
         b_spot_open, b_ema_open, deadline, opened_slot, opened_tx_index, opened_ts, \
         open_signature, rent_protocol_bps, penalty_lender_bps, penalty_protocol_bps, \
         penalty_keeper_bps, expiry_fee_bps, terms_source, armed, s_count, s_since, arm_count, \
         disarm_count, status, exit_slot, exit_ts, exit_signature, exit_keeper, buyback_cost, \
         b_live_exit, rent_paid, rent_protocol, penalty, expiry_fee, keeper_paid, \
         borrower_payout, token_surplus, liquidated_expired, settle_condition, borrower_pnl, \
         duration_s) \
         VALUES ($1, $2, $3, $4::numeric, $5, $6, $7::numeric, $8::numeric, $9::numeric, \
                 $10::numeric, $11::numeric, $12::numeric, $13::numeric, $14::numeric, \
                 $15::numeric, $16::numeric, $17::numeric, $18::numeric, $19::numeric, \
                 $20::numeric, $21, $22, $23, $24, $25, $26, $27, $28, $29, $30, $31, $32, \
                 $33::numeric, $34, $35, $36, $37, $38, $39, $40, $41, $42::numeric, \
                 $43::numeric, $44::numeric, $45::numeric, $46::numeric, $47::numeric, \
                 $48::numeric, $49::numeric, $50::numeric, $51, $52, $53::numeric, $54)",
    )
    .bind(k(&p.position))
    .bind(k(&p.market))
    .bind(k(&p.borrower))
    .bind(n(p.nonce))
    .bind(small(p.tier))
    .bind(small(p.venue))
    .bind(n(p.q))
    .bind(n(p.s))
    .bind(n(p.m))
    .bind(n(p.v_open))
    .bind(n(p.margin_added))
    .bind(n(p.v))
    .bind(n(p.n_open))
    .bind(n(p.fee_open))
    .bind(n(p.venue_rent))
    .bind(n(p.entry_price))
    .bind(n(p.spot_wad_open))
    .bind(n(p.ema_wad_open))
    .bind(n(p.b_spot_open))
    .bind(n(p.b_ema_open))
    .bind(p.deadline)
    .bind(big(p.opened_slot))
    .bind(i32::try_from(p.opened_tx_index)?)
    .bind(p.opened_ts)
    .bind(&p.open_signature)
    .bind(t.map(|t| i16_of(t.rent_protocol_bps)))
    .bind(t.map(|t| i16_of(t.penalty_split[0])))
    .bind(t.map(|t| i16_of(t.penalty_split[1])))
    .bind(t.map(|t| i16_of(t.penalty_split[2])))
    .bind(t.map(|t| i16_of(t.expiry_fee_bps)))
    .bind(t.map(|_| "account"))
    .bind(p.armed)
    .bind((p.arm_count > 0).then(|| n(p.s_count)))
    .bind((p.arm_count > 0).then_some(p.s_since))
    .bind(i64::from(p.arm_count))
    .bind(i64::from(p.disarm_count))
    .bind(p.status)
    .bind(p.exit_slot.map(big))
    .bind(p.exit_ts)
    .bind(&p.exit_signature)
    .bind(ko(p.exit_keeper))
    .bind(p.buyback_cost.map(n))
    .bind(p.b_live_exit.map(n))
    .bind(p.rent_paid.map(n))
    .bind(p.rent_protocol.map(n))
    .bind(p.penalty.map(n))
    .bind(p.expiry_fee.map(n))
    .bind(p.keeper_paid.map(n))
    .bind(p.borrower_payout.map(n))
    .bind(p.token_surplus.map(n))
    .bind(p.liquidated_expired)
    .bind(p.settle_condition.map(small))
    .bind(p.borrower_pnl().map(n))
    .bind(p.exit_ts.map(|e| e - p.opened_ts))
    .execute(&mut **db)
    .await
    .with_context(|| format!("positions {}", p.position))?;
    Ok(())
}

/// The configuration rows from the accounts (`protocol_params_history` with `source =
/// 'account'` at the session's first transaction, `protocol_state`, `venue_guards`).
async fn config_rows(
    db: &mut Transaction<'_, Postgres>,
    state: &ChainState,
    first: &ChainTx,
) -> anyhow::Result<()> {
    let Some(c) = state.config.as_ref() else {
        return Ok(());
    };
    let params = params_json(c);
    sqlx::query(
        "INSERT INTO protocol_params_history (signature, ix_index, slot, tx_index, ts, source, \
         params, tiers, ema, util_kink_bps, governance, pause_authority, coverage_authority, \
         fee_recipient, timelock_s, venue_enabled, pyth_sol_usd, pyth_feed_id) \
         VALUES ($1, 0, $2, $3, $4, 'account', $5, '[]'::jsonb, '{}'::jsonb, $6, $7, $8, $9, $10, \
                 $11, $12, $13, $14)",
    )
    .bind(&first.signature)
    .bind(big(first.slot))
    .bind(i32::try_from(first.tx_index)?)
    .bind(first.block_time)
    .bind(params)
    .bind(i64::from(c.kink.util_kink_bps))
    .bind(k(&c.governance))
    .bind(k(&c.pause_authority))
    .bind(crate::assert::key_or_null(&c.coverage_authority))
    .bind(k(&c.fee_recipient))
    .bind(i64::from(c.timelock_s))
    .bind(i64::from(c.venue_enabled))
    .bind(k(&c.pyth_sol_usd))
    .bind(shortwin_indexer::numeric::hex(&c.pyth_feed_id))
    .execute(&mut **db)
    .await
    .context("protocol_params_history")?;
    sqlx::query(
        "UPDATE protocol_state SET paused_opens = $1, route_broken_bits = $2, venue_enabled = $3",
    )
    .bind(c.paused_opens())
    .bind(i64::from(c.venue_route_broken))
    .bind(i64::from(c.venue_enabled))
    .execute(&mut **db)
    .await?;
    if let Some(g) = state.guard.as_ref() {
        for v in 0..6u8 {
            let i = usize::from(v);
            sqlx::query(
                "INSERT INTO venue_guards (venue, upgraded, guard_slot, programdata_slot, \
                 flagged_at, last_slot, last_ts) \
                 VALUES ($1, $2, $3::numeric, $4::numeric, $5, 0, 0)",
            )
            .bind(small(v))
            .bind(g.upgraded & (1 << v) != 0)
            .bind(n(g.guard_slot[i]))
            .bind(n(g.flagged_slot[i]))
            .bind((g.flagged_at[i] != 0).then_some(g.flagged_at[i]))
            .execute(&mut **db)
            .await?;
        }
    }
    Ok(())
}

/// The analytics-relevant `Params` fields as §4.2 JSON (the full encoding is INGEST's).
pub fn params_json(c: &GlobalConfig) -> serde_json::Value {
    let p = &c.params;
    serde_json::json!({
        "open_fee_bps": p.open_fee_bps,
        "rent_protocol_bps": p.rent_protocol_bps,
        "penalty_lender_bps": p.penalty_lender_bps,
        "penalty_protocol_bps": p.penalty_protocol_bps,
        "penalty_keeper_bps": p.penalty_keeper_bps,
        "expiry_fee_bps": p.expiry_fee_bps,
        "market_creation_fee": p.market_creation_fee.to_string(),
    })
}
