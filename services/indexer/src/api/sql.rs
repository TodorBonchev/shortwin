//! SQL fragments that build the API's JSON objects in Postgres (INDEXER.md §9; `openapi.yaml`
//! schemas). Owner: API.
//!
//! Each macro expands to a string literal, so a query is `concat!(…)` of literals: a
//! `&'static str` that sqlx accepts without `AssertSqlSafe` and prepares once per connection.
//! The encoding is §4.2's: NUMERIC domains (`u64`, `u128`, `i128`, sums) as decimal strings
//! (`::text`), slots, unix seconds and small integers as JSON numbers, keys base58 (`::text`), the
//! program's small enums by name (`sw_api_*`, `0007_api.sql`). Aliases the fragments expect are
//! named in each macro's doc; `st` is always `ingest_state` (for `finalized`, §3.2).

/// `ChainRef` fields of a row aliased `$a` with `$ts` as its time column (expands to the
/// `jsonb_build_object` argument list, without parentheses).
macro_rules! chain_ref_args {
    ($a:literal, $ts:literal) => {
        concat!(
            "'signature', ",
            $a,
            ".signature::text, 'slot', ",
            $a,
            ".slot, 'tx_index', ",
            $a,
            ".tx_index, 'ix_index', ",
            $a,
            ".ix_index, 'ts', ",
            $a,
            ".",
            $ts,
            ", 'finalized', ",
            $a,
            ".slot <= st.finalized_slot"
        )
    };
}

/// `Event` of `events e`.
macro_rules! event_json {
    () => {
        concat!(
            "jsonb_build_object(",
            chain_ref_args!("e", "ev_ts"),
            ", 'id', e.id, 'kind', e.kind, 'invocation', e.invocation, \
             'source', CASE e.ev_source WHEN 0 THEN 'cpi' ELSE 'log' END, \
             'market', e.market::text, 'data', e.data)"
        )
    };
}

/// `MarketSummary` of `v_market_overview o` with `v_sol_usd u` (LEFT JOIN … ON TRUE).
macro_rules! market_summary_json {
    () => {
        "jsonb_build_object(\
         'market', o.market::text, 'mint', o.mint::text, \
         'symbol', left(o.symbol, 16), 'name', left(o.name, 64), 'decimals', o.decimals::int, \
         'venue', sw_api_venue(o.venue), 'tier', sw_api_tier(o.tier), 'created_ts', o.created_ts, \
         'flags', jsonb_build_object('paused_opens', o.paused_opens, \
             'paused_coverage', o.paused_coverage, 'mint_unsafe', o.mint_unsafe, \
             'route_broken', o.route_broken, 'wipe_live', o.wipe_live), \
         'tokens_free', o.tokens_free::text, 'borrowed_q', o.borrowed_q::text, \
         'inventory_tokens', o.inventory_tokens::text, 'util_bps', o.util_bps, \
         'open_positions', o.open_positions::bigint, 'ema_price', o.ema_price::text, \
         'price_ts', o.price_ts, 'oi_lamports', o.oi_lamports::text, \
         'tvl_lamports', o.tvl_lamports::text, \
         'tvl_usd', (o.tvl_lamports * u.usd_per_sol / 1000000000)::float8)"
    };
}

/// `Position` of `positions p` with its market `markets m`.
macro_rules! position_json {
    () => {
        "jsonb_build_object(\
         'position', p.position::text, 'market', p.market::text, 'mint', m.mint::text, \
         'borrower', p.borrower::text, 'nonce', p.nonce::text, 'status', p.status, \
         'tier', sw_api_tier(p.tier), 'venue', sw_api_venue(p.venue), 'q', p.q::text, \
         's', p.s::text, 'm', p.m::text, 'margin_added', p.margin_added::text, 'v', p.v::text, \
         'n_open', p.n_open::text, 'fee_open', p.fee_open::text, \
         'entry_price', p.entry_price::text, 'deadline', p.deadline, 'opened_ts', p.opened_ts, \
         'open_signature', p.open_signature::text, 'armed', p.armed, \
         'exit', CASE WHEN p.status = 'open' THEN NULL ELSE jsonb_build_object(\
             'ts', COALESCE(p.exit_ts, 0), 'signature', p.exit_signature::text, \
             'keeper', p.exit_keeper::text, 'buyback_cost', p.buyback_cost::text, \
             'rent_paid', p.rent_paid::text, 'penalty', p.penalty::text, \
             'expiry_fee', p.expiry_fee::text, 'keeper_paid', p.keeper_paid::text, \
             'borrower_payout', COALESCE(p.borrower_payout, 0)::text, \
             'settle_condition', sw_api_condition(p.settle_condition), \
             'borrower_pnl', COALESCE(p.borrower_pnl, 0)::text, \
             'duration_s', COALESCE(p.duration_s, p.exit_ts - p.opened_ts, 0)) END)"
    };
}

/// `Settlement` of `settlements s` with its market `markets sm`.
macro_rules! settlement_json {
    () => {
        concat!(
            "jsonb_build_object(",
            chain_ref_args!("s", "ts"),
            ", 'position', s.position::text, 'market', s.market::text, 'mint', sm.mint::text, \
             'keeper', s.keeper::text, 'condition', sw_api_condition(s.condition), \
             'q', s.q::text, 'n_open', s.n_open::text, 'expired', s.expired, \
             'b_live', s.b_live::text, 'b_rs', s.b_rs::text, 'b_ema', s.b_ema::text, \
             'p_lo', s.p_lo::text, 'p_hi', s.p_hi::text, 'fv', s.fv::text, 'k_s', s.k_s::text, \
             'lender_amount', s.lender_amount::text, 'lump', s.lump::text, \
             'borrower_amount', s.borrower_amount::text, 'ema_age_s', s.ema_age_s, \
             'p_ref', s.p_ref::text, 'p_ref_source', s.p_ref_source, \
             'token_value', s.token_value::text, 'lender_pnl', s.lender_pnl::text, \
             'realised_lender_loss', s.realised_lender_loss::text)"
        )
    };
}

/// `LenderPosition` (without `share_bps`, which the handler converts across epochs) of
/// `lender_positions lp` with its market `markets m`.
macro_rules! lender_json {
    () => {
        "jsonb_build_object(\
         'market', lp.market::text, 'mint', m.mint::text, 'owner', lp.owner::text, \
         'shares', lp.shares::text, 'epoch', lp.epoch::bigint, 'lock', lp.lock::text, \
         'lock_until', lp.lock_until, 'deposited_tokens', lp.deposited_tokens::text, \
         'withdrawn_tokens', lp.withdrawn_tokens::text, 'sol_paid', lp.sol_paid::text, \
         'pot_draw', lp.pot_draw::text, 'deficit_covered', lp.deficit_covered::text, \
         'deposits', lp.deposits::bigint, 'withdrawals', lp.withdrawals::bigint, \
         'claims', lp.claims::bigint, \
         'is_coin_creator', COALESCE(lp.owner = m.coin_creator, FALSE), \
         'is_market_creator', lp.owner = m.creator, \
         'first_ts', lp.first_ts, 'last_ts', lp.last_ts)"
    };
}

/// The columns a lender row's `share_bps` needs (after [`lender_json`]).
macro_rules! lender_share_cols {
    () => {
        "lp.market::text, lp.shares::text, lp.epoch::bigint, m.share_epoch::bigint, \
         m.total_shares::text"
    };
}

/// `ExitEvent` of an exit event `events e` with `positions p`, `markets m` and (for a
/// `SolSettle`) `settlements s`.
macro_rules! exit_json {
    () => {
        concat!(
            "jsonb_build_object(",
            chain_ref_args!("e", "ev_ts"),
            ", 'kind', CASE e.kind WHEN 'Liquidate' THEN 'liquidation' \
                 WHEN 'Expire' THEN 'expiry' ELSE 'settlement' END, \
             'market', e.market::text, 'mint', m.mint::text, 'position', e.position::text, \
             'keeper', COALESCE(e.actor::text, e.data->>'keeper', p.exit_keeper::text), \
             'q', p.q::text, \
             'buyback_cost', CASE WHEN e.kind = 'SolSettle' THEN NULL ELSE \
                 COALESCE(p.buyback_cost::text, e.data->>'buyback_cost') END, \
             'lender_amount', s.lender_amount::text, \
             'keeper_paid', COALESCE(p.keeper_paid, 0)::text, \
             'borrower_payout', COALESCE(p.borrower_payout, 0)::text, \
             'settle_condition', sw_api_condition(p.settle_condition), \
             'expired', CASE e.kind WHEN 'Liquidate' THEN COALESCE(p.liquidated_expired, FALSE) \
                 WHEN 'Expire' THEN TRUE ELSE COALESCE(s.expired, FALSE) END)"
        )
    };
}

/// `PriceSnapshot` of `market_snapshots ms`.
macro_rules! snapshot_json {
    () => {
        concat!(
            "jsonb_build_object(",
            chain_ref_args!("ms", "ts"),
            ", 'spot_wad', ms.spot_wad::text, 'ema_price', ms.ema_price::text, \
             'ema_liq', ms.ema_liq::text, 'liq_slow', ms.liq_slow::text, \
             'ema_count', ms.ema_count::text, 'tier', sw_api_tier(ms.tier), \
             'util_bps', ms.util_bps::bigint, 'vest_bal', ms.vest_bal::text, \
             'cranker', ms.cranker::text)"
        )
    };
}

/// `VenueTrade` of `venue_trades vt`.
macro_rules! trade_json {
    () => {
        concat!(
            "jsonb_build_object(",
            chain_ref_args!("vt", "ts"),
            ", 'venue', vt.venue, 'pool', vt.pool::text, 'mint', vt.mint::text, 'side', vt.side, \
             'token_amount', vt.token_amount::text, 'sol_amount', vt.sol_amount::text, \
             'price_wad', vt.price_wad::text, \
             'fee_creator_lamports', vt.fee_creator_lamports::text, 'user', vt.user_key::text, \
             'is_shortwin', vt.is_shortwin, 'shortwin_invocation', vt.shortwin_invocation)"
        )
    };
}

/// `FeeLine` of `v_market_fees f`.
macro_rules! fee_line_json {
    () => {
        "jsonb_build_object('fee_type', f.fee_type, 'recipient', f.recipient, 'n', f.n, \
         'total', f.total::text, 'last_24h', f.last_24h::text)"
    };
}

/// `CoverageStats` of `v_market_coverage v`.
macro_rules! coverage_stats_json {
    () => {
        "jsonb_build_object('gaps', v.gaps, 'gap_s_total', v.gap_s_total::bigint, \
         'gap_s_max', v.gap_s_max, 'gaps_24h', v.gaps_24h, 'gap_s_24h', v.gap_s_24h::bigint, \
         'coverage_pauses', v.coverage_pauses, 'coverage_paused_s', v.coverage_paused_s::bigint, \
         'paused_coverage', v.paused_coverage, 'observations_24h', v.observations_24h, \
         'failed_observations_24h', v.failed_observations_24h)"
    };
}

/// `GlobalStats` of `v_global_stats g` with `v_sol_usd u` (LEFT JOIN … ON TRUE).
macro_rules! global_stats_json {
    () => {
        "jsonb_build_object(\
         'markets', g.markets, 'markets_with_positions', g.markets_with_positions, \
         'tvl_lamports', g.tvl_lamports::text, \
         'tvl_usd', (g.tvl_lamports * u.usd_per_sol / 1000000000)::float8, \
         'oi_lamports', g.oi_lamports::text, \
         'notional_open_lamports', g.notional_open_lamports::text, \
         'open_positions', g.open_positions::bigint, 'lender_sol', g.lender_sol::text, \
         'protocol_fees_unswept', g.protocol_fees_unswept::text, \
         'protocol_revenue', g.protocol_revenue::text, \
         'lender_fee_income', g.lender_fee_income::text, \
         'keeper_income', g.keeper_income::text, 'closes', g.closes, \
         'liquidations', g.liquidations, 'expiries', g.expiries, \
         'settlements_s1', g.settlements_s1, 'settlements_s2', g.settlements_s2, \
         'settlements_s3', g.settlements_s3, \
         'realised_lender_loss', g.realised_lender_loss::text, \
         'settlement_lender_pnl', g.settlement_lender_pnl::text, \
         'unique_lenders', g.unique_lenders, 'unique_borrowers', g.unique_borrowers, \
         'repeat_lenders', g.repeat_lenders, 'repeat_borrowers', g.repeat_borrowers, \
         'avg_duration_s', round(g.avg_duration_s, 3)::float8, \
         'coverage_gap_s', g.coverage_gap_s::bigint, \
         'creator_fee_income', g.creator_fee_income::text, \
         'sol_usd', u.usd_per_sol::float8, \
         'inventory_value_lamports', g.inventory_value_lamports::text, \
         'util_bps', g.util_bps, 'rental_fees', g.rental_fees::text, \
         'avg_open_impact_bps', round(g.avg_open_impact_bps, 3)::float8, \
         'avg_exit_slippage_bps', round(g.avg_exit_slippage_bps, 3)::float8)"
    };
}

/// `StatsBucket` of an hourly or daily row `h` whose bucket start is `$t` (counts summed by the
/// daily views are NUMERIC, hence the casts).
macro_rules! stats_bucket_json {
    ($t:literal) => {
        concat!(
            "jsonb_build_object('t', h.", $t, ", 'tvl_lamports', h.tvl_lamports::text, \
             'oi_lamports', h.oi_lamports::text, 'util_bps', h.util_bps::bigint, \
             'open_positions', h.open_positions::bigint, \
             'fees_protocol', h.fees_protocol::text, 'fees_lenders', h.fees_lenders::text, \
             'fees_keeper', h.fees_keeper::text, 'opens', h.opens::bigint, \
             'closes', h.closes::bigint, 'liquidations', h.liquidations::bigint, \
             'expiries', h.expiries::bigint, 'settlements_s1', h.settlements_s1::bigint, \
             'settlements_s2', h.settlements_s2::bigint, \
             'settlements_s3', h.settlements_s3::bigint, \
             'realised_lender_loss', h.realised_lender_loss::text, \
             'shortwin_volume', h.shortwin_volume::text, 'creator_fees', h.creator_fees::text, \
             'coverage_gaps', h.coverage_gaps::bigint, 'coverage_gap_s', h.coverage_gap_s::bigint)"
        )
    };
}

/// An event row with what the WebSocket routes it by: `(id, Event JSON as text, mint,
/// position, actor, kind, market)` of `events e` with `markets m` (LEFT JOIN) and `st`.
macro_rules! ws_event_cols {
    () => {
        concat!(
            "e.id, (",
            event_json!(),
            ")::text, m.mint::text, e.position::text, e.actor::text, e.kind, e.market::text"
        )
    };
}
