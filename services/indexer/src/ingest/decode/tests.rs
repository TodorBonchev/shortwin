//! Decoder unit tests (INDEXER.md §11.5): synthetic transactions built in Rust — every catalog
//! event behind `EVENT_IX_TAG_LE`, attribution at stack heights 1–4, forged "Program data:"
//! lines, the CPMM discriminator from another program, "Log truncated", failed transactions,
//! the derived projection inputs, and v1 key resolution.

use anchor_lang::Discriminator;
use serde_json::Value;
use solana_pubkey::Pubkey;
use solana_signature::Signature;

use super::shortwin_events::tests::every_event;
use super::*;
use crate::ingest::raw::{Commitment, RawTopIx};

fn k(b: u8) -> Pubkey {
    Pubkey::new_from_array([b; 32])
}

fn ix(program: Pubkey, h: u32, data: Vec<u8>, accounts: Vec<Pubkey>) -> RawIx {
    RawIx {
        program_id: program,
        accounts,
        data,
        stack_height: h,
    }
}

fn event_ix(program: Pubkey, h: u32, payload: &[u8]) -> RawIx {
    let mut data = anchor_lang::event::EVENT_IX_TAG_LE.to_vec();
    data.extend_from_slice(payload);
    ix(program, h, data, vec![k(200)])
}

fn raw(instructions: Vec<RawTopIx>, logs: Vec<String>) -> RawTx {
    RawTx {
        signature: Signature::from([7u8; 64]),
        slot: 100,
        tx_index: 2,
        block_time: Some(1_790_000_000),
        commitment: Commitment::Finalized,
        fee_payer: k(50),
        fee: 5_000,
        compute_units: Some(1),
        err: None,
        account_keys: vec![],
        instructions,
        logs,
    }
}

fn sw_ix_data(name: &str) -> Vec<u8> {
    idl::idl()
        .instruction(name)
        .expect(name)
        .discriminator
        .clone()
}

fn b64(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn decode_plain(t: &RawTx) -> Decoded {
    decode(
        t,
        &crate::ingest::ProgramIds::default(),
        &venue::PoolCtx::default(),
        &|_| None,
    )
}

#[test]
fn every_event_is_decoded_from_its_self_cpi_and_attributed_to_its_instruction() {
    let sw = shortwin::ID;
    let market = k(1);
    let events = every_event(market);
    // One crank_price top-level instruction emitting every event (a synthetic carrier).
    let inner: Vec<RawIx> = events.iter().map(|(_, p)| event_ix(sw, 2, p)).collect();
    let t = raw(
        vec![RawTopIx {
            ix: ix(
                sw,
                1,
                sw_ix_data("crank_price"),
                vec![k(9), market, k(10), k(11), k(200), sw],
            ),
            inner,
        }],
        vec![],
    );
    let d = decode_plain(&t);
    assert!(d.quarantine.is_empty(), "{:?}", d.quarantine);
    assert_eq!(d.events.len(), events.len());
    for (e, (name, _)) in d.events.iter().zip(&events) {
        assert_eq!(e.kind, *name);
        assert_eq!(e.invocation, "crank_price");
        assert_eq!(e.ev_source, EvSource::Cpi);
        assert_eq!(e.ev_slot, 10);
        assert_eq!(e.ev_ts, 20);
    }
    assert_eq!(d.invocations.len(), 1);
    assert_eq!(d.invocations[0].market, Some(market));
    assert_eq!(d.shortwin_ixs, 1);
    // PriceCranked's actor is the fee payer.
    let pc = d
        .events
        .iter()
        .find(|e| e.kind == "PriceCranked")
        .expect("PriceCranked");
    assert_eq!(pc.actor, Some(k(50)));
}

#[test]
fn attribution_holds_at_stack_heights_two_to_four() {
    let sw = shortwin::ID;
    let squads = k(60);
    let router = k(61);
    let (_, payload) = every_event(k(1))
        .into_iter()
        .find(|(n, _)| *n == "PauseSet")
        .expect("PauseSet");
    let set_pause = {
        let mut d = sw_ix_data("set_pause");
        d.extend([1u8, 1u8]);
        d
    };
    let t = raw(
        vec![
            // A Squads-style CPI: squads (1) → short.win (2) → event (3).
            RawTopIx {
                ix: ix(squads, 1, vec![1], vec![]),
                inner: vec![
                    ix(sw, 2, set_pause.clone(), vec![k(2), k(3), k(1), k(200), sw]),
                    event_ix(sw, 3, &payload),
                ],
            },
            // Deeper: router (1) → squads (2) → short.win (3) → event (4).
            RawTopIx {
                ix: ix(router, 1, vec![2], vec![]),
                inner: vec![
                    ix(squads, 2, vec![1], vec![]),
                    ix(sw, 3, set_pause, vec![k(2), k(3), k(1), k(200), sw]),
                    event_ix(sw, 4, &payload),
                ],
            },
        ],
        vec![],
    );
    let d = decode_plain(&t);
    assert_eq!(d.events.len(), 2);
    assert_eq!((d.events[0].ix_index, d.events[0].ev_seq), (0, 1));
    assert_eq!((d.events[1].ix_index, d.events[1].ev_seq), (1, 2));
    assert!(d.events.iter().all(|e| e.invocation == "set_pause"));
    let invs: Vec<_> = d
        .invocations
        .iter()
        .map(|i| (i.ix_index, i.inner_index, i.stack_height))
        .collect();
    assert_eq!(invs, vec![(0, Some(0), 2), (1, Some(1), 3)]);
    // No top-level short.win instruction.
    assert_eq!(d.shortwin_ixs, 0);
}

#[test]
fn an_event_tag_not_emitted_by_short_win_itself_is_ignored() {
    let sw = shortwin::ID;
    let (_, payload) = every_event(k(1)).into_iter().next().expect("one");
    // Another program calling short.win with the event tag (the event authority could not
    // sign): its parent is not short.win, so it is no event.
    let t = raw(
        vec![RawTopIx {
            ix: ix(k(70), 1, vec![], vec![]),
            inner: vec![event_ix(sw, 2, &payload)],
        }],
        vec![],
    );
    assert!(decode_plain(&t).events.is_empty());
}

#[test]
fn forged_program_data_lines_are_rejected_and_log_events_attributed() {
    let sw = shortwin::ID;
    let other = k(80);
    let gap = every_event(k(1))
        .into_iter()
        .find(|(n, _)| *n == "CoverageGap")
        .expect("CoverageGap")
        .1;
    let t = raw(
        vec![RawTopIx {
            ix: ix(
                sw,
                1,
                sw_ix_data("crank_price"),
                vec![k(9), k(1), k(10), k(11), k(200), sw],
            ),
            inner: vec![ix(other, 2, vec![], vec![])],
        }],
        vec![
            format!("Program {sw} invoke [1]"),
            format!("Program data: {}", b64(&gap)),
            format!("Program {other} invoke [2]"),
            // `other` prints a short.win event: attributed to `other`, never decoded.
            format!("Program data: {}", b64(&gap)),
            // `other` prints a CPMM SwapEvent: not from the CPMM program, no trade.
            format!("Program data: {}", b64(&venue::CPMM_SWAP_EVENT)),
            format!("Program {other} success"),
            format!("Program {sw} success"),
        ],
    );
    let d = decode_plain(&t);
    assert_eq!(d.events.len(), 1);
    let e = &d.events[0];
    assert_eq!(
        (e.kind, e.ev_source, e.ev_seq),
        ("CoverageGap", EvSource::Log, 1)
    );
    assert_eq!(e.invocation, "crank_price");
    assert!(d.trades.is_empty());
    assert!(d.quarantine.is_empty());
}

#[test]
fn a_truncated_log_stream_is_flagged_and_unknown_payloads_are_quarantined() {
    let sw = shortwin::ID;
    let t = raw(
        vec![RawTopIx {
            ix: ix(sw, 1, sw_ix_data("crank_price"), vec![]),
            // An unknown event discriminator in a self-CPI.
            inner: vec![event_ix(sw, 2, &[1, 2, 3, 4, 5, 6, 7, 8, 9])],
        }],
        vec![
            format!("Program {sw} invoke [1]"),
            format!("Program data: {}", b64(&[9, 9, 9, 9, 9, 9, 9, 9])),
            "Log truncated".into(),
        ],
    );
    let d = decode_plain(&t);
    assert!(d.logs_truncated);
    assert!(d.events.is_empty());
    assert_eq!(d.quarantine.len(), 2);
    assert!(d.quarantine.iter().any(|q| q.ev_source == EvSource::Cpi));
    assert!(d.quarantine.iter().any(|q| q.ev_source == EvSource::Log));
}

#[test]
fn a_failed_transaction_keeps_its_invocations_and_no_events() {
    let sw = shortwin::ID;
    let (_, payload) = every_event(k(1)).into_iter().next().expect("one");
    let mut t = raw(
        vec![RawTopIx {
            ix: ix(sw, 1, sw_ix_data("sol_settle"), vec![]),
            inner: vec![event_ix(sw, 2, &payload)],
        }],
        vec![],
    );
    t.err = Some(serde_json::json!({"InstructionError": [0, {"Custom": 6042}]}));
    let d = decode_plain(&t);
    assert!(d.events.is_empty());
    assert_eq!(d.invocations.len(), 1);
    assert_eq!(d.invocations[0].name, "sol_settle");
    assert_eq!((d.err_ix_index, d.err_code), (Some(0), Some(6042)));
}

#[test]
fn derived_inputs_record_donation_candidate_kind_and_the_config_payload() {
    let sw = shortwin::ID;
    let market = k(1);
    let rec = k(30);
    let vault = k(31);
    let all = every_event(market);
    let payload = |name: &str| all.iter().find(|(n, _)| *n == name).expect(name).1.clone();
    // sol_settle's accounts: keeper, config, market, position, borrower, exposure, market_vault,
    // mint, share_epoch_rec, system_program, event_authority, program.
    let settle_accounts = vec![
        k(40),
        k(41),
        market,
        k(42),
        k(43),
        k(44),
        vault,
        k(45),
        rec,
        SYSTEM_PROGRAM,
        k(200),
        sw,
    ];
    let transfer = |from: Pubkey, to: Pubkey, lamports: u64| {
        let mut d = 2u32.to_le_bytes().to_vec();
        d.extend(lamports.to_le_bytes());
        ix(SYSTEM_PROGRAM, 2, d, vec![from, to])
    };
    let mut propose_pool = sw_ix_data("propose_pool");
    propose_pool.push(5);
    let change = shortwin::state::ConfigChange::Kink {
        util_kink_bps: 7_000,
    };
    let mut propose_config = sw_ix_data("propose_config");
    propose_config.extend(super::borsh_bytes(&change));
    let (kind, enc) = change.encode().expect("encode");
    let hash = shortwin::state::change_hash(kind, &enc);
    let proposed = shortwin::events::ConfigProposed {
        market: Pubkey::default(),
        slot: 1,
        ts: 2,
        kind,
        hash,
        not_before: 3,
    };
    let mut proposed_payload =
        <shortwin::events::ConfigProposed as Discriminator>::DISCRIMINATOR.to_vec();
    proposed_payload.extend(super::borsh_bytes(&proposed));
    let t = raw(
        vec![
            RawTopIx {
                ix: ix(sw, 1, sw_ix_data("sol_settle"), settle_accounts),
                inner: vec![
                    transfer(rec, vault, 777),
                    transfer(k(40), rec, 1_000), // the rent payment: not a donation
                    event_ix(sw, 2, &payload("EpochRolled")),
                ],
            },
            RawTopIx {
                ix: ix(
                    sw,
                    1,
                    propose_pool,
                    vec![k(41), k(46), market, k(45), k(47), k(200), sw],
                ),
                inner: vec![event_ix(sw, 2, &payload("PoolSwitched"))],
            },
            RawTopIx {
                ix: ix(sw, 1, propose_config, vec![k(48), k(41), k(200), sw]),
                inner: vec![event_ix(sw, 2, &proposed_payload)],
            },
        ],
        vec![],
    );
    let d = decode_plain(&t);
    assert_eq!(d.events.len(), 3);
    assert_eq!(
        d.events[0].derived.as_ref().expect("derived")["record_donation"],
        "777"
    );
    assert_eq!(
        d.events[1].derived.as_ref().expect("derived")["candidate_kind"],
        5
    );
    let cfg = d.events[2].derived.as_ref().expect("derived");
    assert_eq!(cfg["payload"]["Kink"]["util_kink_bps"], 7_000);
    assert_eq!(
        cfg["payload_hash"],
        Value::String(crate::numeric::hex(&hash))
    );
    assert_eq!(
        d.events[2].data["hash"],
        Value::String(crate::numeric::hex(&hash))
    );
}

#[test]
fn a_v1_transaction_converts() {
    use solana_message::{MessageHeader, VersionedMessage, v1};
    use solana_transaction::versioned::VersionedTransaction;
    let payer = k(1);
    let program = shortwin::ID;
    let mut msg = v1::Message {
        header: MessageHeader {
            num_required_signatures: 1,
            num_readonly_signed_accounts: 0,
            num_readonly_unsigned_accounts: 1,
        },
        config: Default::default(),
        lifetime_specifier: solana_hash::Hash::default(),
        account_keys: vec![payer, program],
        instructions: vec![solana_message::compiled_instruction::CompiledInstruction {
            program_id_index: 1,
            accounts: vec![0],
            data: sw_ix_data("crank_price"),
        }],
    };
    msg.config.compute_unit_limit = Some(200_000);
    let vt = VersionedTransaction {
        signatures: vec![Signature::from([3u8; 64])],
        message: VersionedMessage::V1(msg),
    };
    let bytes = crate::ingest::rpc::tests::wincode_bytes(&vt);
    let json = serde_json::json!({
        "slot": 9,
        "blockTime": 1,
        "transaction": [b64(&bytes), "base64"],
        "meta": {"err": null, "status": {"Ok": null}, "fee": 5000, "preBalances": [], "postBalances": [],
                 "innerInstructions": [], "logMessages": [], "computeUnitsConsumed": 7},
        "version": 1
    });
    let t: solana_transaction_status_client_types::EncodedConfirmedTransactionWithStatusMeta =
        serde_json::from_value(json).expect("rpc shape");
    let r = crate::ingest::rpc::to_raw_tx(&t, 0, Commitment::Finalized)
        .expect("a v1 transaction converts");
    assert_eq!(r.instructions[0].ix.program_id, program);
    assert_eq!(r.fee_payer, payer);
    let d = decode_plain(&r);
    assert_eq!(d.invocations[0].name, "crank_price");
}

/// Every committed LiteSVM session (INGEST's `tests/data/ingest/`, REPLAY's
/// `tests/data/sessions/`; INDEXER.md §11.3) decodes without a short.win payload refused or
/// read on a prefix: its events have the program's current layouts. A layout change (an
/// appended event field, as `SolSettle.vest_flushed`) must migrate or re-record them, or the
/// ingest tests quarantine their events. No database needed.
#[test]
fn the_committed_sessions_decode_with_the_current_layouts() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let mut files = 0;
    let mut events = 0;
    for dir in ["ingest", "sessions"] {
        let mut paths: Vec<_> = std::fs::read_dir(root.join(dir))
            .expect("tests/data")
            .map(|e| e.expect("entry").path())
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .collect();
        paths.sort();
        for path in paths {
            let s = crate::ingest::session::read(&path).expect("session");
            let ids = crate::ingest::replay::program_ids(&s).expect("program ids");
            for t in &s.txs {
                let r = crate::ingest::rpc::to_raw_tx(
                    &t.transaction,
                    t.tx_index,
                    Commitment::Finalized,
                )
                .expect("converts");
                let d = decode(&r, &ids, &venue::PoolCtx::default(), &|_| None);
                let refused: Vec<&str> = d
                    .quarantine
                    .iter()
                    .filter(|q| q.program == ids.shortwin)
                    .map(|q| q.reason.as_str())
                    .collect();
                assert!(
                    refused.is_empty(),
                    "{}: {}: {refused:?}",
                    path.display(),
                    r.signature
                );
                events += d.events.len();
            }
            files += 1;
        }
    }
    // 13 INGEST fixtures (pump-final P6: the synthetic-exit capture replaced the straddle
    // bundle's two) and REPLAY's 2.
    assert!(
        files >= 15 && events > 1_000,
        "{files} sessions, {events} events"
    );
}
