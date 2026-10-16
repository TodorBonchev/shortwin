//! The venue decoders on recorded mainnet transactions (M10 GC-F1; INDEXER.md §7.2, §11.5): WP-9's
//! gapcal fixtures (`scripts/gapcal/tests/fixtures/*.json.gz`, `record_fixtures.py`: about 20 real
//! transactions per venue, each venue's from one pool, with the pool facts the checks need) through
//! the production conversion (`rpc::to_raw_tx`), the decoder and, for CPMM, the writer. A venue
//! trade's stored post-trade reserves are the venue's pricing reserves:
//!
//! - CPMM: the 20 consecutive swaps of one pool chain exactly in `venue_trades` (each swap's stored
//!   reserves are the next swap's `*_vault_before`), and `vault − reserves` (the fee counters)
//!   grows by exactly the protocol and fund shares of each trade fee plus its creator fee; the
//!   rates come from the venue admin watch when it observed the config (the value at or before the
//!   transaction's slot, else the earliest), and with the rates zeroed — M8's formula — every link
//!   breaks by exactly the counters' share;
//! - PumpSwap: base = the base vault, quote = the quote vault + `virtual_quote_reserves`, and the
//!   event's pre-trade reserves on that basis (not the vault alone) price every sale exactly;
//! - AMM v4: the `ray_log` reserves are the vaults net of `need_take_pnl` (a pump graduate's pool
//!   with none, SOL/USDC with both sides non-zero) and chain;
//! - pump: the curve's virtual reserves chain by each trade's curve SOL and token amounts.

mod ingest_common;

use std::io::Read;

use base64::Engine;
use serde_json::Value;
use shortwin_indexer::ingest::decode::logs::{self, LogPayload};
use shortwin_indexer::ingest::decode::venue::{self, CpmmFeeRates, PoolCtx, PoolInfo, Side, Trade};
use shortwin_indexer::ingest::decode::{self, Decoded};
use shortwin_indexer::ingest::raw::{Commitment, RawTx};
use shortwin_indexer::ingest::writer::Writer;
use shortwin_indexer::ingest::{ProgramIds, refresher, rpc};
use solana_message::compiled_instruction::CompiledInstruction;
use solana_message::{MessageHeader, VersionedMessage, legacy, v0, v1};
use solana_pubkey::Pubkey;
use sqlx::PgPool;

// ---- fixtures ------------------------------------------------------------------------------------

/// A gapcal fixture (`scripts/gapcal/tests/fixtures/<name>`).
fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/../../scripts/gapcal/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let mut text = String::new();
    flate2::read::GzDecoder::new(file)
        .read_to_string(&mut text)
        .unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn key(v: &Value) -> Pubkey {
    v.as_str()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("a base58 key: {v}"))
}

fn u8_of(v: &Value) -> u8 {
    u8::try_from(v.as_u64().expect("an integer")).expect("u8")
}

fn bytes_of(v: &Value) -> Vec<u8> {
    v.as_array().expect("an array").iter().map(u8_of).collect()
}

/// The `getTransaction` JSON of a fixture (`encoding: json`) re-encoded as the indexer fetches it
/// (`encoding: base64`): the message rebuilt from its JSON (legacy, v0 or v1), then
/// `rpc::to_raw_tx` — the production conversion — with the transaction's block position.
fn raw_tx(tx: &Value) -> RawTx {
    let m = &tx["transaction"]["message"];
    let h = &m["header"];
    let header = MessageHeader {
        num_required_signatures: u8_of(&h["numRequiredSignatures"]),
        num_readonly_signed_accounts: u8_of(&h["numReadonlySignedAccounts"]),
        num_readonly_unsigned_accounts: u8_of(&h["numReadonlyUnsignedAccounts"]),
    };
    let account_keys: Vec<Pubkey> = m["accountKeys"]
        .as_array()
        .expect("accountKeys")
        .iter()
        .map(key)
        .collect();
    let blockhash: solana_hash::Hash = m["recentBlockhash"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .expect("recentBlockhash");
    let instructions: Vec<CompiledInstruction> = m["instructions"]
        .as_array()
        .expect("instructions")
        .iter()
        .map(|ix| CompiledInstruction {
            program_id_index: u8_of(&ix["programIdIndex"]),
            accounts: bytes_of(&ix["accounts"]),
            data: bs58::decode(ix["data"].as_str().expect("data"))
                .into_vec()
                .expect("base58 data"),
        })
        .collect();
    let message = match &tx["version"] {
        Value::String(v) if v == "legacy" => VersionedMessage::Legacy(legacy::Message {
            header,
            account_keys,
            recent_blockhash: blockhash,
            instructions,
        }),
        v if v.as_u64() == Some(0) => VersionedMessage::V0(v0::Message {
            header,
            account_keys,
            recent_blockhash: blockhash,
            instructions,
            address_table_lookups: m["addressTableLookups"]
                .as_array()
                .map(|l| {
                    l.iter()
                        .map(|t| v0::MessageAddressTableLookup {
                            account_key: key(&t["accountKey"]),
                            writable_indexes: bytes_of(&t["writableIndexes"]),
                            readonly_indexes: bytes_of(&t["readonlyIndexes"]),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }),
        v if v.as_u64() == Some(1) => {
            let c = &m["transactionConfig"];
            let u32_at = |f: &str| c[f].as_u64().map(|v| u32::try_from(v).expect("u32"));
            let mut msg = v1::Message {
                header,
                config: Default::default(),
                lifetime_specifier: blockhash,
                account_keys,
                instructions,
            };
            msg.config.priority_fee = c["priorityFee"].as_u64();
            msg.config.compute_unit_limit = u32_at("computeUnitLimit");
            msg.config.loaded_accounts_data_size_limit = u32_at("loadedAccountsDataSizeLimit");
            msg.config.heap_size = u32_at("heapSize");
            VersionedMessage::V1(msg)
        }
        v => panic!("transaction version {v}"),
    };
    let signatures: Vec<Vec<u8>> = tx["transaction"]["signatures"]
        .as_array()
        .expect("signatures")
        .iter()
        .map(|s| {
            bs58::decode(s.as_str().expect("a signature"))
                .into_vec()
                .expect("base58")
        })
        .collect();
    // The wire format: legacy and v0 put the signatures (short-vec count) first; v1 (SIMD-0385)
    // puts the message first, then the header's signatures without a count.
    let mut wire = Vec::new();
    if let VersionedMessage::V1(_) = &message {
        wire.extend(message.serialize());
        signatures.iter().for_each(|s| wire.extend(s));
    } else {
        wire.push(u8::try_from(signatures.len()).expect("< 128 signatures"));
        signatures.iter().for_each(|s| wire.extend(s));
        wire.extend(message.serialize());
    }
    let mut rpc_json = tx.clone();
    rpc_json["transaction"] = serde_json::json!([
        base64::engine::general_purpose::STANDARD.encode(&wire),
        "base64"
    ]);
    let t: solana_transaction_status_client_types::EncodedConfirmedTransactionWithStatusMeta =
        serde_json::from_value(rpc_json).expect("the getTransaction shape");
    let tx_index =
        u32::try_from(tx["transactionIndex"].as_u64().expect("transactionIndex")).expect("u32");
    let raw = rpc::to_raw_tx(&t, tx_index, Commitment::Finalized).expect("to_raw_tx");
    assert_eq!(
        raw.signature.to_string(),
        tx["transaction"]["signatures"][0]
            .as_str()
            .expect("signature"),
        "the rebuilt transaction keeps its signature"
    );
    raw
}

/// `meta.{pre,post}TokenBalances` amount of the token account `account` (`when` = "pre" / "post").
fn token_balance(tx: &Value, raw: &RawTx, account: &Pubkey, when: &str) -> u64 {
    let index = raw
        .account_keys
        .iter()
        .position(|k| k == account)
        .unwrap_or_else(|| panic!("{account} not in the transaction"));
    tx["meta"][format!("{when}TokenBalances")]
        .as_array()
        .expect("token balances")
        .iter()
        .find(|b| b["accountIndex"].as_u64() == Some(index as u64))
        .and_then(|b| b["uiTokenAmount"]["amount"].as_str())
        .and_then(|a| a.parse().ok())
        .unwrap_or_else(|| panic!("no {when} balance of {account}"))
}

fn decoded(raw: &RawTx, pools: &PoolCtx) -> Decoded {
    let d = decode::decode(raw, &ProgramIds::default(), pools, &|_| None);
    assert!(!d.logs_truncated, "{}", raw.signature);
    d
}

/// The trades of `venue` on `pool`, in the decoder's order.
fn trades_on(d: &Decoded, venue: &str, pool: &Pubkey) -> Vec<Trade> {
    d.trades
        .iter()
        .filter(|t| t.trade.venue == venue && t.trade.pool == *pool)
        .map(|t| t.trade.clone())
        .collect()
}

/// `(sol, token)` stored reserves.
fn reserves(t: &Trade) -> (u64, u64) {
    (
        t.pool_sol_reserve.expect("a SOL reserve"),
        t.pool_token_reserve.expect("a token reserve"),
    )
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().expect("8 bytes"))
}

// ---- CPMM ----------------------------------------------------------------------------------------

/// One CPMM `SwapEvent` as the venue emitted it (decoded here, independently of the indexer).
#[derive(Clone, Copy, Debug)]
struct CpmmEvent {
    buy: bool,
    /// `(sol, token)` `*_vault_before`: the pricing reserves the swap priced on.
    before: (u64, u64),
    trade_fee: u64,
    creator_fee: u64,
    creator_fee_on_input: bool,
}

fn cpmm_events(raw: &RawTx, pool: &Pubkey) -> Vec<CpmmEvent> {
    logs::parse(raw)
        .entries
        .iter()
        .filter(|e| e.program == venue::RAYDIUM_CPMM)
        .filter_map(|e| match &e.payload {
            LogPayload::Data(b) if b.starts_with(&venue::CPMM_SWAP_EVENT) => Some(b[8..].to_vec()),
            _ => None,
        })
        .filter(|b| b[..32] == pool.to_bytes())
        .map(|b| {
            let (in_before, out_before) = (u64_at(&b, 32), u64_at(&b, 40));
            let o = 32 + 6 * 8 + 1;
            let buy = b[o..o + 32] == venue::WSOL.to_bytes();
            CpmmEvent {
                buy,
                before: if buy {
                    (in_before, out_before)
                } else {
                    (out_before, in_before)
                },
                trade_fee: u64_at(&b, o + 64),
                creator_fee: u64_at(&b, o + 72),
                creator_fee_on_input: b[o + 80] != 0,
            }
        })
        .collect()
}

/// The CPMM fixture: the transactions, the pool, its config and vaults.
struct CpmmFixture {
    txs: Vec<(Value, RawTx)>,
    pool: Pubkey,
    config: Pubkey,
    rates: CpmmFeeRates,
    sol_vault: Pubkey,
    tok_vault: Pubkey,
}

fn cpmm_fixture() -> CpmmFixture {
    let fx = fixture("cpmm.json.gz");
    let rates = CpmmFeeRates {
        protocol: fx["rates"][0].as_u64().expect("rate"),
        fund: fx["rates"][1].as_u64().expect("rate"),
    };
    CpmmFixture {
        txs: fx["txs"]
            .as_array()
            .expect("txs")
            .iter()
            .map(|t| (t.clone(), raw_tx(t)))
            .collect(),
        pool: key(&fx["pool"]),
        config: key(&fx["amm_config"]),
        rates,
        sol_vault: key(&fx["sol_vault"]),
        tok_vault: key(&fx["tok_vault"]),
    }
}

/// Writes every fixture transaction through the writer into `pool`, returns the pool's stored
/// `(sol, token)` reserves in chain order.
async fn write_cpmm(db: &PgPool, fx: &CpmmFixture) -> Vec<(u64, u64)> {
    let mut w = Writer::open(db, ProgramIds::default(), "rpc_backfill")
        .await
        .expect("writer");
    for (_, raw) in &fx.txs {
        w.write(raw).await.expect("write");
    }
    w.close().await.expect("close");
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT pool_sol_reserve::text, pool_token_reserve::text FROM venue_trades \
         WHERE pool = $1 ORDER BY slot, tx_index, ix_index, ev_source, ev_seq",
    )
    .bind(fx.pool.to_string())
    .fetch_all(db)
    .await
    .expect("venue_trades");
    rows.into_iter()
        .map(|(s, t)| (s.parse().expect("u64"), t.parse().expect("u64")))
        .collect()
}

/// `stored[i]` against the next swap's `*_vault_before`: the per-link differences (0 = chained).
fn cpmm_links(stored: &[(u64, u64)], events: &[CpmmEvent]) -> Vec<(i128, i128)> {
    stored
        .iter()
        .zip(&events[1..])
        .map(|(s, e)| {
            (
                i128::from(s.0) - i128::from(e.before.0),
                i128::from(s.1) - i128::from(e.before.1),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cpmm_stored_reserves_are_the_pricing_reserves_and_chain() {
    let fx = cpmm_fixture();
    let events: Vec<CpmmEvent> = fx
        .txs
        .iter()
        .flat_map(|(_, raw)| cpmm_events(raw, &fx.pool))
        .collect();
    assert_eq!(events.len(), 20);
    assert!(events.iter().any(|e| e.buy) && events.iter().any(|e| !e.buy));
    // A creator fee in every swap, on the input in some and on the output in others.
    assert!(events.iter().all(|e| e.creator_fee > 0));
    assert!(
        events.iter().any(|e| e.creator_fee_on_input)
            && events.iter().any(|e| !e.creator_fee_on_input),
        "creator fees on both sides"
    );
    assert_eq!(fx.rates, CpmmFeeRates::MAINNET);
    let pg = ingest_common::pg().await;

    // 1. No watched config (the default rates, the fixture config's): every link chains exactly.
    let db = pg.fresh_db().await.expect("db");
    let stored = write_cpmm(&db, &fx).await;
    assert_eq!(stored.len(), 20);
    assert!(
        cpmm_links(&stored, &events).iter().all(|l| *l == (0, 0)),
        "{:?}",
        cpmm_links(&stored, &events)
    );
    // `vault − reserves` after each transaction is the fee counters: never negative, and growing
    // by exactly the protocol and fund shares of the trade fee (on the input) and the creator fee
    // (on its side).
    let mut k = 0;
    let mut prev: Option<(u64, u64)> = None;
    for (tx, raw) in &fx.txs {
        let n = cpmm_events(raw, &fx.pool).len();
        let last = stored[k + n - 1];
        let counters = (
            token_balance(tx, raw, &fx.sol_vault, "post")
                .checked_sub(last.0)
                .expect("SOL counters ≥ 0"),
            token_balance(tx, raw, &fx.tok_vault, "post")
                .checked_sub(last.1)
                .expect("token counters ≥ 0"),
        );
        if let Some(p) = prev {
            let mut grow = (0u64, 0u64);
            for e in &events[k..k + n] {
                let cut = fx.rates.counters_share(e.trade_fee);
                let creator_on_sol = e.buy == e.creator_fee_on_input;
                if e.buy {
                    grow.0 += cut;
                } else {
                    grow.1 += cut;
                }
                if creator_on_sol {
                    grow.0 += e.creator_fee;
                } else {
                    grow.1 += e.creator_fee;
                }
            }
            assert_eq!(
                (counters.0 - p.0, counters.1 - p.1),
                grow,
                "{}",
                raw.signature
            );
        }
        prev = Some(counters);
        k += n;
    }

    // 2. The watch observed zero rates before the window and the real ones after it: the swaps
    // take the value at or before their slot, so the stored reserves are M8's (the protocol and
    // fund shares still counted) and every link breaks by exactly that share, on the input side.
    let first = fx.txs[0].1.slot;
    let last = fx.txs[fx.txs.len() - 1].1.slot;
    let db = pg.fresh_db().await.expect("db");
    let watched = [
        (first - 1, venue::CPMM_PROTOCOL_FEE_RATE, "0"),
        (first - 1, venue::CPMM_FUND_FEE_RATE, "0"),
        (last + 1, venue::CPMM_PROTOCOL_FEE_RATE, "120000"),
        (last + 1, venue::CPMM_FUND_FEE_RATE, "40000"),
    ];
    for (slot, field, value) in watched {
        refresher::record_admin_value(&db, &fx.config, field, slot, "raydium_cpmm", None, value)
            .await
            .expect("watched");
    }
    let m8 = write_cpmm(&db, &fx).await;
    for (l, e) in cpmm_links(&m8, &events).iter().zip(&events) {
        let cut = i128::from(fx.rates.counters_share(e.trade_fee));
        assert!(cut > 0);
        assert_eq!(*l, if e.buy { (cut, 0) } else { (0, cut) });
    }

    // 3. Only observed after the window: the earliest observation applies (here the real rates
    // on the protocol side, 0 on the fund side, so each link breaks by the fund share alone).
    let db = pg.fresh_db().await.expect("db");
    refresher::record_admin_value(
        &db,
        &fx.config,
        venue::CPMM_PROTOCOL_FEE_RATE,
        last + 1,
        "raydium_cpmm",
        None,
        "120000",
    )
    .await
    .expect("watched");
    refresher::record_admin_value(
        &db,
        &fx.config,
        venue::CPMM_FUND_FEE_RATE,
        last + 1,
        "raydium_cpmm",
        None,
        "0",
    )
    .await
    .expect("watched");
    let fund_only = write_cpmm(&db, &fx).await;
    for (l, e) in cpmm_links(&fund_only, &events).iter().zip(&events) {
        let fund = i128::from(e.trade_fee) * 40_000 / 1_000_000;
        assert_eq!(*l, if e.buy { (fund, 0) } else { (0, fund) });
    }
    pg.stop().await.expect("stop");
}

// ---- PumpSwap ------------------------------------------------------------------------------------

/// One pAMM `BuyEvent` / `SellEvent` (the fields the checks use, decoded here).
#[derive(Clone, Copy, Debug)]
struct PammEvent {
    sell: bool,
    base_amount: u64,
    pool_base: u64,
    pool_quote: u64,
    /// `quote_amount_out` of a sale (before fees).
    quote_amount: u64,
}

fn pamm_events(raw: &RawTx, pool: &Pubkey) -> Vec<PammEvent> {
    let buy = venue::anchor_event_discriminator("BuyEvent");
    let sell = venue::anchor_event_discriminator("SellEvent");
    let mut out = Vec::new();
    for top in &raw.instructions {
        for ix in &top.inner {
            let Some(p) = ix.data.strip_prefix(anchor_lang::event::EVENT_IX_TAG_LE) else {
                continue;
            };
            if ix.program_id != venue::PUMP_AMM || !(p.starts_with(&buy) || p.starts_with(&sell)) {
                continue;
            }
            let b = &p[8..];
            if b[112..144] != pool.to_bytes() {
                continue;
            }
            out.push(PammEvent {
                sell: p.starts_with(&sell),
                base_amount: u64_at(b, 8),
                pool_base: u64_at(b, 40),
                pool_quote: u64_at(b, 48),
                quote_amount: u64_at(b, 56),
            });
        }
    }
    out
}

#[test]
fn pump_swap_quote_reserves_are_the_effective_reserves() {
    let fx = fixture("pamm.json.gz");
    let pool = key(&fx["pool"]);
    let (base_vault, quote_vault) = (key(&fx["base_vault"]), key(&fx["quote_vault"]));
    let (mut sales, mut buys, mut links) = (0, 0, 0);
    // Every trade's stored quote reserve − the quote vault after it: the virtual quote reserves.
    let mut vqrs = Vec::new();
    // The previous transaction's last stored reserves and its vaults after it.
    let mut prev: Option<((u64, u64), (u64, u64))> = None;
    for tx in fx["txs"].as_array().expect("txs") {
        let raw = raw_tx(tx);
        let tr = trades_on(&decoded(&raw, &PoolCtx::default()), "pump_swap", &pool);
        let ev = pamm_events(&raw, &pool);
        assert!(!tr.is_empty() && tr.len() == ev.len(), "{}", raw.signature);
        let vaults = |when| {
            (
                token_balance(tx, &raw, &quote_vault, when),
                token_balance(tx, &raw, &base_vault, when),
            )
        };
        assert_eq!((ev[0].pool_quote, ev[0].pool_base), vaults("pre"));
        // A transaction trading right after the previous one on this pool (its vaults are where
        // the previous one left them): the previous stored reserves are this one's pre-trade
        // effective reserves, so the virtual quote reserves carry across transactions too.
        if let Some((stored, after)) = prev
            && after == vaults("pre")
        {
            links += 1;
            assert_eq!(stored, (ev[0].pool_quote + vqrs[0], ev[0].pool_base));
        }
        for (i, (t, e)) in tr.iter().zip(&ev).enumerate() {
            assert_eq!(t.mint, Some(key(&fx["mint"])));
            // After each trade: base = the base vault, quote = the quote vault + the virtual
            // quote reserves (the next trade's pre-trade vaults, or the vaults after the
            // transaction for its last trade).
            let (quote_after, base_after) = ev
                .get(i + 1)
                .map_or_else(|| vaults("post"), |n| (n.pool_quote, n.pool_base));
            let (sol, tok) = reserves(t);
            assert_eq!(tok, base_after);
            vqrs.push(sol.checked_sub(quote_after).expect("quote ≥ the vault"));
            // A sale prices on the effective quote reserves (vault + virtual), not on the vault.
            if e.sell {
                sales += 1;
                let vqr = *vqrs.last().expect("pushed");
                let price = |q: u64| {
                    u128::from(q) * u128::from(e.base_amount)
                        / (u128::from(e.pool_base) + u128::from(e.base_amount))
                };
                assert_eq!(u128::from(e.quote_amount), price(e.pool_quote + vqr));
                assert_ne!(u128::from(e.quote_amount), price(e.pool_quote));
                assert_eq!(t.side, Side::Sell);
            } else {
                buys += 1;
                assert_eq!(t.side, Side::Buy);
            }
        }
        prev = Some((reserves(tr.last().expect("a trade")), vaults("post")));
    }
    // One value in every trade, buys and sells, over the fixture's slots: on this pool no trade
    // moved the virtual quote reserves (the pre-trade value each one priced on is the post-trade
    // one stored), and it is the boosted pool's: 17.6 SOL, the value the events' tails carry
    // (decoded independently of the indexer, in Python, by the M10 GC-F1 review).
    assert!(vqrs.iter().all(|v| *v == vqrs[0]), "{vqrs:?}");
    assert_eq!(vqrs[0], 17_600_247_874);
    assert!(
        vqrs.len() >= 20 && sales >= 10 && buys >= 1 && links >= 10,
        "{} trades, {sales} sales, {buys} buys, {links} links",
        vqrs.len()
    );
}

// ---- AMM v4 --------------------------------------------------------------------------------------

/// The pool (account 1) of the AMM v4 swap that printed `e`.
fn swap_pool(raw: &RawTx, e: &logs::LogEntry) -> Option<Pubkey> {
    let top = raw.instructions.get(e.ix_index)?;
    let ix = match e.node? {
        None => &top.ix,
        Some(j) => top.inner.get(j)?,
    };
    ix.accounts.get(1).copied()
}

#[test]
fn ammv4_reserves_are_the_vaults_net_of_need_take_pnl_and_chain() {
    let fx = fixture("ammv4.json.gz");
    let mut counts = Vec::new();
    let mut pnl_seen = 0;
    for p in fx["pools"].as_array().expect("pools") {
        let pool = key(&p["pool"]);
        let sol_is_coin = p["sol_is_coin"].as_bool().expect("bool");
        let (sol_vault, tok_vault) = (key(&p["sol_vault"]), key(&p["tok_vault"]));
        let pnl = (
            p["need_take_pnl_coin"].as_u64().expect("u64"),
            p["need_take_pnl_pc"].as_u64().expect("u64"),
        );
        let (sol_pnl, tok_pnl) = if sol_is_coin { pnl } else { (pnl.1, pnl.0) };
        pnl_seen = pnl_seen.max(sol_pnl.min(tok_pnl));
        let mut ctx = PoolCtx::default();
        ctx.pools.insert(
            pool,
            PoolInfo {
                mint: Pubkey::new_from_array([7; 32]),
                market: None,
                sol_is_first: Some(sol_is_coin),
            },
        );
        // `(stored after, ray_log before)` of every swap, in chain order.
        let mut flat: Vec<((u64, u64), (u64, u64))> = Vec::new();
        for tx in p["txs"].as_array().expect("txs") {
            let raw = raw_tx(tx);
            let tr = trades_on(&decoded(&raw, &ctx), "raydium_v4", &pool);
            let before: Vec<(u64, u64)> = logs::parse(&raw)
                .entries
                .iter()
                .filter(|e| e.program == venue::RAYDIUM_V4 && swap_pool(&raw, e) == Some(pool))
                .filter_map(|e| match &e.payload {
                    LogPayload::RayLog(b) => venue::ray_log(b),
                    _ => None,
                })
                .map(|l| {
                    if sol_is_coin {
                        (l.pool_coin, l.pool_pc)
                    } else {
                        (l.pool_pc, l.pool_coin)
                    }
                })
                .collect();
            assert!(!tr.is_empty(), "{}", raw.signature);
            let last = reserves(tr.last().expect("a trade"));
            assert_eq!(
                (
                    token_balance(tx, &raw, &sol_vault, "post") - last.0,
                    token_balance(tx, &raw, &tok_vault, "post") - last.1
                ),
                (sol_pnl, tok_pnl),
                "{}",
                raw.signature
            );
            assert_eq!(before.len(), tr.len(), "{}", raw.signature);
            flat.extend(tr.iter().map(reserves).zip(before));
        }
        for (a, b) in flat.iter().zip(&flat[1..]) {
            assert_eq!(a.0, b.1);
        }
        counts.push(flat.len());
    }
    assert_eq!(counts[0], 20);
    assert!(counts[1] >= 3, "{counts:?}");
    assert!(pnl_seen > 0, "the SOL/USDC check is not vacuous");
}

// ---- pump ----------------------------------------------------------------------------------------

#[test]
fn pump_curve_virtual_reserves_chain() {
    let fx = fixture("pump.json.gz");
    let mint = key(&fx["mint"]);
    let curve = venue::bonding_curve_of(&mint);
    assert_eq!(curve, key(&fx["curve"]));
    let flat: Vec<Trade> = fx["txs"]
        .as_array()
        .expect("txs")
        .iter()
        .flat_map(|tx| {
            trades_on(
                &decoded(&raw_tx(tx), &PoolCtx::default()),
                "pump_curve",
                &curve,
            )
        })
        .collect();
    assert_eq!(flat.len(), 20);
    for (a, b) in flat.iter().zip(&flat[1..]) {
        let fees = b.fee_protocol.expect("fee") + b.fee_creator.expect("creator fee");
        let (sa, ta) = reserves(a);
        let (sb, tb) = reserves(b);
        // The curve moves by the trade's SOL net of the fees and by its tokens.
        match b.side {
            Side::Buy => {
                assert_eq!(sb - sa, b.sol_amount - fees);
                assert_eq!(ta - tb, b.token_amount);
            }
            Side::Sell => {
                assert_eq!(sa - sb, b.sol_amount + fees);
                assert_eq!(tb - ta, b.token_amount);
            }
        }
    }
}
