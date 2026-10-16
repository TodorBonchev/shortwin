//! Decoding (INDEXER.md §5, §7.2). Owner: INGEST.
//!
//! For a [`RawTx`]:
//!
//! 1. the invocation tree: every top-level and inner instruction with its stack height; each
//!    short.win invocation's name and arguments (Anchor discriminator ‖ Borsh) — at any stack
//!    height, so a multisig-executed `propose_config` (Squads CPI) decodes like a top-level one
//!    (§5.2);
//! 2. `emit_cpi!` events: inner instructions of short.win whose data starts with
//!    `anchor_lang::event::EVENT_IX_TAG_LE`, then the event discriminator, then Borsh, whose
//!    parent (one stack level up) is a short.win invocation — the event authority PDA must sign
//!    the self-CPI, which only short.win can do; the event belongs to that parent (§5.2);
//! 3. log events ([`logs`]): "Program data:" lines attributed through the
//!    "invoke [n]" / "success" / "failed" stack to the program executing — accepted only from
//!    short.win (`CoverageGap`, an exit's `MintSafetyTripped`) and from the venue programs for
//!    their own events (§7.2), so another program's forged log line is never taken for a
//!    short.win event (the retired straddle helper `complete_curve`'s events are no longer
//!    ingested: design pump-final §9, owner decision D2);
//! 4. venue trades and lifecycle events ([`venue`]); upgrades of venue programs.
//!
//! A payload that does not decode is quarantined (`ingest_quarantine`), never dropped (§10.3).
//! Failed transactions are recorded with their invocations only (§3.6): their inner
//! instructions up to the failure are never read as events.
//!
//! Projection inputs no event field carries are attached to the event as `derived` (stored in
//! `events.derived`, so a rebuild needs nothing but `events`): `record_donation` of an
//! `EpochRolled` (§6.4), `candidate_kind` of `propose_pool`'s pool events, the decoded payload of a
//! `ConfigProposed` (§5.4).

use serde_json::{Value, json};
use solana_pubkey::Pubkey;

use super::ProgramIds;
use super::raw::{RawIx, RawTx};
use crate::model::EvSource;

pub mod idl;
pub mod logs;
pub mod shortwin_events;
pub mod venue;

use logs::{LogPayload, Node};
use shortwin_events as sw;

/// The System program (the record-donation transfer, §6.4).
pub const SYSTEM_PROGRAM: Pubkey = solana_pubkey::pubkey!("11111111111111111111111111111111");

/// One short.win invocation (`invocations`).
#[derive(Clone, Debug, PartialEq)]
pub struct Invocation {
    pub ix_index: usize,
    /// `None` for the top-level instruction, else its index in the inner list.
    pub inner_index: Option<usize>,
    pub stack_height: u32,
    pub program: Pubkey,
    pub name: String,
    pub market: Option<Pubkey>,
    pub args: Option<Value>,
}

/// One decoded short.win event (`events`).
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub ix_index: usize,
    pub ev_source: EvSource,
    pub ev_seq: usize,
    pub program: Pubkey,
    /// The emitting instruction's name.
    pub invocation: String,
    pub kind: &'static str,
    pub market: Option<Pubkey>,
    pub position: Option<Pubkey>,
    pub actor: Option<Pubkey>,
    pub ev_slot: u64,
    pub ev_ts: i64,
    pub data: Value,
    /// Projection inputs from the same transaction that no event field carries.
    pub derived: Option<Value>,
}

/// A decoded venue trade with its position in the transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TradeRow {
    pub ix_index: usize,
    pub ev_source: EvSource,
    pub ev_seq: usize,
    pub trade: venue::Trade,
    pub market: Option<Pubkey>,
    pub is_shortwin: bool,
    pub shortwin_invocation: Option<String>,
}

/// A decoded venue lifecycle event with its position.
#[derive(Clone, Debug, PartialEq)]
pub struct VenueEventRow {
    pub ix_index: usize,
    pub ev_source: EvSource,
    pub ev_seq: usize,
    pub event: venue::LifecycleEvent,
    pub market: Option<Pubkey>,
}

/// A venue program upgrade with its position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpgradeRow {
    pub ix_index: usize,
    pub inner_index: Option<usize>,
    pub upgrade: venue::ProgramUpgrade,
}

/// An undecodable payload (`ingest_quarantine`), or a short.win event longer than the known
/// layout whose prefix was applied (`applied`, M8-SEC-13).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quarantined {
    pub ix_index: usize,
    pub ev_source: EvSource,
    pub ev_seq: usize,
    pub program: Pubkey,
    pub reason: String,
    pub raw: Vec<u8>,
    /// The known prefix was decoded and applied (a newer layout); `false`: not applied at all.
    pub applied: bool,
    /// The market the payload names, when readable.
    pub market: Option<Pubkey>,
}

/// Everything decoded from one transaction.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Decoded {
    pub invocations: Vec<Invocation>,
    pub events: Vec<Event>,
    pub trades: Vec<TradeRow>,
    pub venue_events: Vec<VenueEventRow>,
    pub upgrades: Vec<UpgradeRow>,
    pub quarantine: Vec<Quarantined>,
    pub logs_truncated: bool,
    /// The failing top-level instruction and its custom error code (failed transactions).
    pub err_ix_index: Option<i16>,
    pub err_code: Option<i64>,
    /// Top-level short.win instructions.
    pub shortwin_ixs: i16,
    /// AMM v4 orientations learned from short.win legs: `(pool, mint, market, sol_is_coin)`.
    pub learned_pools: Vec<(Pubkey, Pubkey, Option<Pubkey>, bool)>,
}

/// The invocation-tree node `node` of top-level instruction `top`.
fn node_ix(top: &super::raw::RawTopIx, node: Node) -> Option<&RawIx> {
    match node {
        None => Some(&top.ix),
        Some(j) => top.inner.get(j),
    }
}

/// The parent of inner instruction `j` (the nearest preceding node one stack level up).
fn parent_of(top: &super::raw::RawTopIx, j: usize) -> Option<Node> {
    let h = top.inner.get(j)?.stack_height;
    if h <= 2 {
        return Some(None);
    }
    (0..j)
        .rev()
        .find(|&i| top.inner[i].stack_height + 1 == h)
        .map(Some)
}

/// The ancestors of `node`, nearest first (excluding `node` itself).
fn ancestors(top: &super::raw::RawTopIx, node: Node) -> Vec<Node> {
    let mut out = Vec::new();
    let mut cur = node;
    while let Some(j) = cur {
        match parent_of(top, j) {
            Some(p) => {
                out.push(p);
                cur = p;
            }
            None => break,
        }
    }
    out
}

fn is_event_cpi(ix: &RawIx, program: &Pubkey) -> bool {
    ix.program_id == *program
        && ix.stack_height >= 2
        && ix.data.starts_with(anchor_lang::event::EVENT_IX_TAG_LE)
}

/// The custom error of `err` (`{"InstructionError": [k, {"Custom": code}]}`).
pub fn instruction_error(err: &Value) -> (Option<i16>, Option<i64>) {
    let Some(arr) = err.get("InstructionError").and_then(Value::as_array) else {
        return (None, None);
    };
    let ix = arr
        .first()
        .and_then(Value::as_u64)
        .and_then(|v| i16::try_from(v).ok());
    let code = arr
        .get(1)
        .and_then(|e| e.get("Custom"))
        .and_then(Value::as_i64);
    (ix, code)
}

/// Decodes `raw` (module docs). `pools`: what `venue_pools` knows (AMM v4 orientation, pool
/// mints) and the CPMM config rates the venue admin watch observed; `market_of_mint`: the
/// indexed markets by mint (third-party trades name a mint, not a market).
pub fn decode(
    raw: &RawTx,
    ids: &ProgramIds,
    pools: &venue::PoolCtx,
    market_of_mint: &dyn Fn(&Pubkey) -> Option<Pubkey>,
) -> Decoded {
    let mut out = Decoded::default();
    let sw_id = ids.shortwin;
    if let Some(err) = &raw.err {
        let (i, c) = instruction_error(err);
        out.err_ix_index = i;
        out.err_code = c;
    }

    // ---- 1. short.win invocations (every height), named and decoded ----
    // For each top-level instruction, its short.win nodes and their decoded instruction.
    let mut sw_nodes: Vec<Vec<(Node, sw::DecodedIx)>> = Vec::with_capacity(raw.instructions.len());
    for (k, top) in raw.instructions.iter().enumerate() {
        let mut nodes = Vec::new();
        if top.ix.program_id == sw_id {
            out.shortwin_ixs = out.shortwin_ixs.saturating_add(1);
        }
        let all = std::iter::once((None, &top.ix))
            .chain(top.inner.iter().enumerate().map(|(j, ix)| (Some(j), ix)));
        for (node, ix) in all {
            if ix.program_id != sw_id || is_event_cpi(ix, &sw_id) {
                continue;
            }
            let d = sw::decode_instruction(&sw_id, &ix.data, &ix.accounts);
            if let Some(reason) = &d.error {
                out.quarantine.push(Quarantined {
                    ix_index: k,
                    ev_source: EvSource::Instruction,
                    ev_seq: node.map_or(0, |j| j + 1),
                    program: sw_id,
                    reason: format!("{}: {reason}", d.name),
                    raw: ix.data.clone(),
                    applied: false,
                    market: d.market,
                });
            }
            out.invocations.push(Invocation {
                ix_index: k,
                inner_index: node,
                stack_height: ix.stack_height,
                program: sw_id,
                name: d.name.clone(),
                market: d.market,
                args: d.args.clone(),
            });
            nodes.push((node, d));
        }
        sw_nodes.push(nodes);
    }
    // Venue program upgrades (recorded for failed transactions too? no: an upgrade that failed
    // did not happen).
    let parsed = logs::parse(raw);
    out.logs_truncated = parsed.truncated;
    if !raw.ok() {
        return out;
    }
    for (k, top) in raw.instructions.iter().enumerate() {
        let all = std::iter::once((None, &top.ix))
            .chain(top.inner.iter().enumerate().map(|(j, ix)| (Some(j), ix)));
        for (node, ix) in all {
            if let Some(u) = venue::venue_upgrade(&ix.program_id, &ix.data, &ix.accounts) {
                out.upgrades.push(UpgradeRow {
                    ix_index: k,
                    inner_index: node,
                    upgrade: u,
                });
            }
        }
    }

    let sw_name_of = |k: usize, node: Node| -> Option<&sw::DecodedIx> {
        sw_nodes
            .get(k)?
            .iter()
            .find(|(n, _)| *n == node)
            .map(|(_, d)| d)
    };
    // The short.win invocation at or below `node` (itself or its nearest short.win ancestor).
    let sw_frame = |k: usize, node: Node| -> Option<(Node, &sw::DecodedIx)> {
        let top = raw.instructions.get(k)?;
        std::iter::once(node)
            .chain(ancestors(top, node))
            .find_map(|n| sw_name_of(k, n).map(|d| (n, d)))
    };
    let accounts_of = |k: usize, node: Node| -> &[Pubkey] {
        raw.instructions
            .get(k)
            .and_then(|t| node_ix(t, node))
            .map_or(&[], |ix| ix.accounts.as_slice())
    };

    // ---- 2. emit_cpi! events (short.win) and venue CPI events (pump, pAMM) ----
    for (k, top) in raw.instructions.iter().enumerate() {
        for (j, ix) in top.inner.iter().enumerate() {
            let Some(payload) = ix.data.strip_prefix(anchor_lang::event::EVENT_IX_TAG_LE) else {
                continue;
            };
            let Some(parent) = parent_of(top, j) else {
                continue;
            };
            let parent_ix = node_ix(top, parent);
            if parent_ix.is_none_or(|p| p.program_id != ix.program_id) {
                // A self-CPI's parent is the emitting program itself.
                continue;
            }
            if ix.program_id == sw_id {
                let invocation =
                    sw_name_of(k, parent).map_or_else(|| "unknown".to_string(), |d| d.name.clone());
                match sw::decode_shortwin_event(payload) {
                    Ok(ev) => {
                        if ev.trailing > 0 {
                            out.quarantine.push(longer_layout(
                                k,
                                EvSource::Cpi,
                                j,
                                sw_id,
                                &ev,
                                payload,
                            ));
                        }
                        let derived =
                            derive(raw, k, parent, ev.kind, &invocation, sw_name_of(k, parent));
                        out.events.push(make_event(
                            raw,
                            k,
                            EvSource::Cpi,
                            j,
                            sw_id,
                            invocation,
                            ev,
                            derived,
                        ));
                    }
                    Err(e) => out.quarantine.push(Quarantined {
                        ix_index: k,
                        ev_source: EvSource::Cpi,
                        ev_seq: j,
                        program: sw_id,
                        reason: format!("{e:#}"),
                        raw: payload.to_vec(),
                        applied: false,
                        market: sw::payload_market(payload),
                    }),
                }
            } else if ix.program_id == venue::PUMP || ix.program_id == venue::PUMP_AMM {
                let Some(name) = venue::venue_event_name(&ix.program_id, payload) else {
                    continue;
                };
                let body = &payload[8..];
                let frame = sw_frame(k, parent);
                let sw_market = frame.and_then(|(_, d)| d.market);
                let sw_mint =
                    frame.and_then(|(n, d)| sw::account_of(&d.name, "mint", accounts_of(k, n)));
                let venue_accounts = accounts_of(k, parent);
                let trade = match (ix.program_id == venue::PUMP, name) {
                    // `TradeEvent.ix_name` is a label only (v3 trades carry `buy` / `sell` /
                    // `buy_exact_quote_in`, multi-hop ones `multi_hop_swap`): never dispatched on.
                    (true, "TradeEvent") => venue::pump_trade(body),
                    (true, "PostCompleteBuyEvent") => venue::pump_post_complete_buy(body),
                    (false, "BuyEvent" | "SellEvent") => {
                        let data = node_ix(top, parent).map_or(&[][..], |p| p.data.as_slice());
                        let (mut base, mut quote) =
                            venue::pump_amm_trade_mints(data, venue_accounts, body);
                        if base.is_none() && quote.is_none() {
                            // A pool no hop group names: the known pool's mint against WSOL.
                            let known =
                                venue::pump_amm_event_pool(body).and_then(|p| pools.pools.get(&p));
                            if let Some(info) = known {
                                (base, quote) = if info.sol_is_first == Some(true) {
                                    (Some(venue::WSOL), Some(info.mint))
                                } else {
                                    (Some(info.mint), Some(venue::WSOL))
                                };
                            }
                        }
                        venue::pump_amm_trade(name == "BuyEvent", body, base, quote)
                    }
                    _ => {
                        let ev = venue::lifecycle(&ix.program_id, name, body);
                        let market =
                            sw_market.or_else(|| ev.mint.as_ref().and_then(market_of_mint));
                        out.venue_events.push(VenueEventRow {
                            ix_index: k,
                            ev_source: EvSource::Cpi,
                            ev_seq: j,
                            event: ev,
                            market,
                        });
                        continue;
                    }
                };
                let Some(mut trade) = trade else {
                    continue;
                };
                if trade.mint.is_none() {
                    trade.mint = sw_mint.or_else(|| pools.pools.get(&trade.pool).map(|p| p.mint));
                }
                push_trade(&mut out, k, EvSource::Cpi, j, trade, frame.map(|(_, d)| d));
            }
        }
    }

    // ---- 3. log events: short.win "Program data:", CPMM SwapEvent, ray_log ----
    for entry in &parsed.entries {
        let k = entry.ix_index;
        match &entry.payload {
            LogPayload::Data(bytes) if entry.program == sw_id => {
                let node = entry.node.unwrap_or(None);
                let invocation =
                    sw_name_of(k, node).map_or_else(|| "unknown".to_string(), |d| d.name.clone());
                match sw::decode_shortwin_event(bytes) {
                    Ok(ev) => {
                        if ev.trailing > 0 {
                            out.quarantine.push(longer_layout(
                                k,
                                EvSource::Log,
                                entry.line,
                                sw_id,
                                &ev,
                                bytes,
                            ));
                        }
                        out.events.push(make_event(
                            raw,
                            k,
                            EvSource::Log,
                            entry.line,
                            sw_id,
                            invocation,
                            ev,
                            None,
                        ));
                    }
                    Err(e) => out.quarantine.push(Quarantined {
                        ix_index: k,
                        ev_source: EvSource::Log,
                        ev_seq: entry.line,
                        program: sw_id,
                        reason: format!("{e:#}"),
                        raw: bytes.clone(),
                        applied: false,
                        market: sw::payload_market(bytes),
                    }),
                }
            }
            LogPayload::Data(bytes)
                if entry.program == venue::RAYDIUM_CPMM
                    && bytes.starts_with(&venue::CPMM_SWAP_EVENT) =>
            {
                // `swap_base_input` / `swap_base_output`: [payer, authority, amm_config,
                // pool_state, …]; the venue checks `amm_config == pool_state.amm_config`.
                let accounts = entry.node.map(|n| accounts_of(k, n)).unwrap_or_default();
                let user = accounts.first().copied().unwrap_or(raw.fee_payer);
                let rates = pools.cpmm_rates(accounts.get(2));
                let Some(trade) = venue::cpmm_swap(bytes, user, rates) else {
                    continue;
                };
                let frame = below_frame(&entry.below, &sw_id).and_then(|n| sw_frame(k, n));
                push_trade(
                    &mut out,
                    k,
                    EvSource::Log,
                    entry.line,
                    trade,
                    frame.map(|(_, d)| d),
                );
            }
            LogPayload::RayLog(bytes) if entry.program == venue::RAYDIUM_V4 => {
                let Some(log) = venue::ray_log(bytes) else {
                    continue;
                };
                let accounts = entry.node.map(|n| accounts_of(k, n)).unwrap_or_default();
                let Some(&pool) = accounts.get(1) else {
                    continue;
                };
                let swap_data = entry
                    .node
                    .and_then(|n| raw.instructions.get(k).and_then(|t| node_ix(t, n)))
                    .map(|ix| ix.data.first().copied());
                // `SwapBaseInV2` / `SwapBaseOutV2` (tags 16, 17): user_owner is account 7; the
                // legacy swaps end with it.
                let user = match swap_data.flatten() {
                    Some(16 | 17) => accounts.get(7).copied(),
                    _ => accounts.last().copied(),
                }
                .unwrap_or(raw.fee_payer);
                let frame = below_frame(&entry.below, &sw_id).and_then(|n| sw_frame(k, n));
                let known = pools.pools.get(&pool).and_then(|p| p.sol_is_first);
                let from_leg =
                    frame.and_then(|(_, d)| venue::ray_v4_orientation_from_leg(&log, &d.name));
                let Some(sol_is_coin) = known.or(from_leg) else {
                    continue;
                };
                let mut trade = venue::ray_v4_trade(&log, pool, user, sol_is_coin);
                let sw_mint =
                    frame.and_then(|(n, d)| sw::account_of(&d.name, "mint", accounts_of(k, n)));
                trade.mint = sw_mint.or_else(|| pools.pools.get(&pool).map(|p| p.mint));
                if known.is_none()
                    && let (Some(mint), Some(sol_is_coin)) = (trade.mint, from_leg)
                {
                    out.learned_pools.push((
                        pool,
                        mint,
                        frame.and_then(|(_, d)| d.market),
                        sol_is_coin,
                    ));
                }
                push_trade(
                    &mut out,
                    k,
                    EvSource::Log,
                    entry.line,
                    trade,
                    frame.map(|(_, d)| d),
                );
            }
            _ => {}
        }
    }
    // Chain order within the transaction (§3.3): by instruction, CPI events before log events.
    out.events
        .sort_by_key(|e| (e.ix_index, e.ev_source as i16, e.ev_seq));
    out
}

/// The nearest short.win frame in a log line's stack (below the venue's frame).
fn below_frame(below: &[(Pubkey, Option<Node>)], sw_id: &Pubkey) -> Option<Node> {
    below
        .iter()
        .rev()
        .find(|(p, n)| p == sw_id && n.is_some())
        .and_then(|(_, n)| *n)
}

/// The `ingest_quarantine` record of a short.win event decoded on its known prefix.
fn longer_layout(
    k: usize,
    ev_source: EvSource,
    ev_seq: usize,
    program: Pubkey,
    ev: &sw::EventPayload,
    payload: &[u8],
) -> Quarantined {
    tracing::warn!(
        kind = ev.kind,
        trailing = ev.trailing,
        "a short.win event longer than the indexer's layout: its known prefix is applied"
    );
    Quarantined {
        ix_index: k,
        ev_source,
        ev_seq,
        program,
        reason: format!(
            "{}: {} bytes after the known layout (a newer program version?); the known prefix \
             was applied",
            ev.kind, ev.trailing
        ),
        raw: payload.to_vec(),
        applied: true,
        market: sw::payload_market(payload),
    }
}

fn push_trade(
    out: &mut Decoded,
    k: usize,
    ev_source: EvSource,
    ev_seq: usize,
    trade: venue::Trade,
    shortwin: Option<&sw::DecodedIx>,
) {
    if trade.mint.is_none() {
        return; // a pool the indexer cannot place (not a listed market's)
    }
    // A short.win leg belongs to the market of its short.win instruction. Anyone else's trade
    // belongs to a market only when it trades one of that market's pools: the writer decides
    // that from the projection (`sw_is_market_pool`), never from the mint alone — any pool can
    // name a listed mint (M8-SEC-6).
    let market = shortwin.and_then(|d| d.market);
    out.trades.push(TradeRow {
        ix_index: k,
        ev_source,
        ev_seq,
        is_shortwin: shortwin.is_some(),
        shortwin_invocation: shortwin.map(|d| d.name.clone()),
        market,
        trade,
    });
}

#[allow(clippy::too_many_arguments)]
fn make_event(
    raw: &RawTx,
    k: usize,
    ev_source: EvSource,
    ev_seq: usize,
    program: Pubkey,
    invocation: String,
    ev: sw::EventPayload,
    derived: Option<Value>,
) -> Event {
    let market = sw::key_field(&ev.data, "market");
    let has_clock = ev.data.get("slot").is_some();
    Event {
        ix_index: k,
        ev_source,
        ev_seq,
        program,
        invocation,
        kind: ev.kind,
        market,
        position: sw::position_of(&ev.data),
        actor: sw::actor_of(ev.kind, &ev.data, &raw.fee_payer),
        ev_slot: if has_clock {
            sw::u64_field(&ev.data, "slot")
        } else {
            raw.slot
        },
        ev_ts: if has_clock {
            sw::i64_field(&ev.data, "ts")
        } else {
            raw.block_time.unwrap_or(0)
        },
        data: ev.data,
        derived,
    }
}

/// Projection inputs no event field carries (module docs).
fn derive(
    raw: &RawTx,
    k: usize,
    emitter: Node,
    kind: &str,
    invocation: &str,
    emitter_ix: Option<&sw::DecodedIx>,
) -> Option<Value> {
    let top = raw.instructions.get(k)?;
    let emitter_raw = node_ix(top, emitter)?;
    match (kind, invocation) {
        ("EpochRolled", "sol_settle") => {
            // §6.4: lamports pre-funded at the record's address, swept into the vault by
            // `arming::create_pda_account` (a System transfer, inner to this invocation, from
            // `share_epoch_rec` to `market_vault`).
            let rec = sw::account_of("sol_settle", "share_epoch_rec", &emitter_raw.accounts)?;
            let vault = sw::account_of("sol_settle", "market_vault", &emitter_raw.accounts)?;
            let h = emitter_raw.stack_height;
            let start = emitter.map_or(0, |j| j + 1);
            let mut donation: u64 = 0;
            for ix in top.inner.iter().skip(start) {
                if ix.stack_height <= h {
                    break; // left the invocation's subtree
                }
                if ix.program_id == SYSTEM_PROGRAM
                    && ix.stack_height == h + 1
                    && ix.data.len() == 12
                    && ix.data[..4] == 2u32.to_le_bytes()
                    && ix.accounts.first() == Some(&rec)
                    && ix.accounts.get(1) == Some(&vault)
                {
                    let mut b = [0u8; 8];
                    b.copy_from_slice(&ix.data[4..12]);
                    donation = donation.saturating_add(u64::from_le_bytes(b));
                }
            }
            Some(json!({ "record_donation": donation.to_string() }))
        }
        ("PoolProposed" | "PoolSwitched", "propose_pool") => {
            let kind = emitter_ix?.args.as_ref()?.get("candidate_kind")?.as_u64()?;
            Some(json!({ "candidate_kind": kind }))
        }
        ("ConfigProposed", "propose_config") => Some(match sw::config_change(&emitter_raw.data) {
            Ok((kind, payload, hash)) => json!({
                "change_kind": kind,
                "payload": payload,
                "payload_hash": hash,
            }),
            Err(e) => json!({ "payload_error": format!("{e:#}") }),
        }),
        _ => None,
    }
}

/// Borsh bytes of a value (tests build synthetic payloads with the program's own types).
#[cfg(test)]
pub(crate) fn borsh_bytes<T: anchor_lang::AnchorSerialize>(v: &T) -> Vec<u8> {
    let mut out = Vec::new();
    v.serialize(&mut out).expect("borsh");
    out
}

#[cfg(test)]
mod tests;
