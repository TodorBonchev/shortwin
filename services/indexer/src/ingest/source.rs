//! Ingest sources (INDEXER.md §2). FOUNDATION: the trait is frozen; INGEST implements it for
//! the RPC backfill (`rpc`), the live `logsSubscribe` stream (`live`), Yellowstone gRPC (behind
//! the same trait, §2.4) and recorded sessions (`replay`).

use super::raw::{Commitment, RawTx};

/// What a source yields, in chain order within the source (§3.3).
#[derive(Clone, Debug, PartialEq)]
pub enum SourceItem {
    /// A transaction (successful or failed) that mentions a tracked address.
    Tx(Box<RawTx>),
    /// Every transaction of the source's addresses up to and including `slot` has been
    /// yielded at `commitment` (advances `ingest_state` and the cursors, §3.5).
    Watermark { slot: u64, commitment: Commitment },
    /// A previously yielded slot is no longer on the chain at `confirmed` (a fork switch the
    /// source observed): roll back every transaction at or above it that is not finalized
    /// (§3.4).
    Rollback { slot: u64 },
}

/// A stream of [`SourceItem`]s.
///
/// `async fn` in a trait: the writer drives one source at a time on one task, so the futures
/// need not be `Send` (as `shortwin_services_common::Chain`).
#[allow(async_fn_in_trait)]
pub trait TxSource {
    /// `ingest_txs.source` of what this source yields (`model::SOURCES`).
    fn name(&self) -> &'static str;

    /// The next item; `Ok(None)` when a finite source (backfill, replay) is exhausted. A live
    /// source never returns `None`; transport errors are retried inside the source and only
    /// surface after its retry budget (§2.3).
    async fn next(&mut self) -> anyhow::Result<Option<SourceItem>>;
}
