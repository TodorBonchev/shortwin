//! Decoding events (ARCH §3.10): `emit_cpi!` events from a transaction's inner instructions,
//! and the `emit!` log events (`CoverageGap`, an exit path's `MintSafetyTripped`) from its
//! `Program data:` log lines.
//!
//! An event CPI's data is `EVENT_IX_TAG_LE ‖ event discriminator ‖ Borsh(event)`; a log
//! event's line is `Program data: base64(event discriminator ‖ Borsh(event))`.

use anchor_lang::__private::base64::{self, Engine};
use anchor_lang::{AnchorDeserialize, Discriminator, Event};
use litesvm::types::TransactionMetadata;
use solana_message::inner_instruction::InnerInstruction;

fn payloads<'a>(inner: impl Iterator<Item = &'a InnerInstruction>) -> Vec<Vec<u8>> {
    let tag = anchor_lang::event::EVENT_IX_TAG_LE;
    inner
        .filter_map(|inner| {
            let data = &inner.instruction.data;
            data.strip_prefix(tag).map(<[u8]>::to_vec)
        })
        .collect()
}

fn decode<T: Event + Discriminator + AnchorDeserialize>(payloads: Vec<Vec<u8>>) -> Vec<T> {
    payloads
        .into_iter()
        .filter_map(|payload| {
            let body = payload.strip_prefix(T::DISCRIMINATOR)?;
            T::try_from_slice(body).ok()
        })
        .collect()
}

/// Every event CPI payload of the transaction (discriminator + Borsh bytes), in order.
pub fn raw(meta: &TransactionMetadata) -> Vec<Vec<u8>> {
    payloads(meta.inner_instructions.iter().flatten())
}

/// Every event of type `T` the transaction emitted, in order.
pub fn of<T: Event + Discriminator + AnchorDeserialize>(meta: &TransactionMetadata) -> Vec<T> {
    decode(raw(meta))
}

/// Every event CPI payload of top-level instruction `ix` (0-based) of the transaction.
pub fn raw_of_ix(meta: &TransactionMetadata, ix: usize) -> Vec<Vec<u8>> {
    meta.inner_instructions
        .get(ix)
        .map(|inner| payloads(inner.iter()))
        .unwrap_or_default()
}

/// Every event of type `T` that top-level instruction `ix` (0-based) emitted, in order.
pub fn of_ix<T: Event + Discriminator + AnchorDeserialize>(
    meta: &TransactionMetadata,
    ix: usize,
) -> Vec<T> {
    decode(raw_of_ix(meta, ix))
}

/// Every `emit!` log event of type `T` in the transaction's logs, in order.
pub fn logged<T: Event + Discriminator + AnchorDeserialize>(meta: &TransactionMetadata) -> Vec<T> {
    let engine = base64::engine::general_purpose::STANDARD;
    decode(
        meta.logs
            .iter()
            .filter_map(|line| line.strip_prefix("Program data: "))
            .filter_map(|b64| engine.decode(b64.trim()).ok())
            .collect(),
    )
}

/// The single event of type `T` (panics unless exactly one was emitted).
pub fn one<T: Event + Discriminator + AnchorDeserialize>(meta: &TransactionMetadata) -> T {
    let mut all = of::<T>(meta);
    assert_eq!(
        all.len(),
        1,
        "expected exactly one {} event, logs:\n{}",
        core::any::type_name::<T>(),
        meta.pretty_logs()
    );
    all.remove(0)
}
