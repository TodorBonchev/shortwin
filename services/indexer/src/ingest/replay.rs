//! The recorded-session source (INDEXER.md §2.5, §11.3). Owner: INGEST.
//!
//! [`SessionSource`] yields `session.txs[range]` through `rpc::to_raw_tx` at `finalized`
//! (`source = "replay"`), with the session header's program ids; [`ingest_session`] drives it
//! through the writer, and each checkpoint stands in for the account refresher (its short.win
//! accounts are written to `account_snapshots` exactly as the live refresher would write them,
//! §2.6) once the transaction it follows is ingested.

use std::ops::Range;

use anyhow::Context;
use sqlx::PgPool;

use super::raw::Commitment;
use super::session::Session;
use super::source::{SourceItem, TxSource};
use super::writer::Writer;
use super::{IngestReport, ProgramIds};

/// The session header's program id.
pub fn program_ids(session: &Session) -> anyhow::Result<ProgramIds> {
    Ok(ProgramIds {
        shortwin: crate::numeric::parse_key(&session.header.program_id)
            .context("header program_id")?,
    })
}

/// A recorded session as a [`TxSource`] (finite; finalized).
pub struct SessionSource<'a> {
    session: &'a Session,
    next: usize,
    end: usize,
    commitment: Commitment,
}

impl<'a> SessionSource<'a> {
    /// `session.txs[range]` at `finalized`.
    pub fn new(session: &'a Session, range: Range<usize>) -> Self {
        Self::with_commitment(session, range, Commitment::Finalized)
    }

    /// `session.txs[range]` at `commitment` (tests replay at `confirmed` to exercise the
    /// finality tracker).
    pub fn with_commitment(
        session: &'a Session,
        range: Range<usize>,
        commitment: Commitment,
    ) -> Self {
        let end = range.end.min(session.txs.len());
        Self {
            session,
            next: range.start.min(end),
            end,
            commitment,
        }
    }

    /// Index of the next transaction.
    pub fn position(&self) -> usize {
        self.next
    }
}

impl TxSource for SessionSource<'_> {
    fn name(&self) -> &'static str {
        "replay"
    }

    async fn next(&mut self) -> anyhow::Result<Option<SourceItem>> {
        if self.next >= self.end {
            return Ok(None);
        }
        let t = &self.session.txs[self.next];
        self.next += 1;
        let raw = super::rpc::to_raw_tx(&t.transaction, t.tx_index, self.commitment)
            .with_context(|| format!("session tx {} (slot {})", self.next - 1, t.slot))?;
        Ok(Some(SourceItem::Tx(Box::new(raw))))
    }
}

/// See [`super::ingest_session`].
pub async fn ingest_session(
    pool: &PgPool,
    session: &Session,
    range: Range<usize>,
) -> anyhow::Result<IngestReport> {
    ingest_session_at(pool, session, range, Commitment::Finalized).await
}

/// [`ingest_session`] at a chosen commitment.
pub async fn ingest_session_at(
    pool: &PgPool,
    session: &Session,
    range: Range<usize>,
    commitment: Commitment,
) -> anyhow::Result<IngestReport> {
    let ids = program_ids(session)?;
    let mut writer = Writer::open(pool, ids, "replay").await?;
    let res = drive(pool, session, range, commitment, &mut writer).await;
    let report = writer.close().await;
    res?;
    report
}

async fn drive(
    pool: &PgPool,
    session: &Session,
    range: Range<usize>,
    commitment: Commitment,
    writer: &mut Writer,
) -> anyhow::Result<()> {
    let ids = writer.ids();
    let start = range.start;
    // A checkpoint taken before the first transaction of the range.
    for (k, cp) in &session.checkpoints {
        if *k == start && start == 0 {
            super::refresher::apply_checkpoint(pool, cp, &ids.shortwin).await?;
        }
    }
    let mut src = SessionSource::with_commitment(session, range, commitment);
    loop {
        let i = src.position();
        match src.next().await? {
            None => break,
            Some(SourceItem::Tx(raw)) => {
                writer.write(&raw).await?;
                for (k, cp) in &session.checkpoints {
                    if *k == i + 1 {
                        super::refresher::apply_checkpoint(pool, cp, &ids.shortwin).await?;
                    }
                }
            }
            Some(SourceItem::Watermark { .. } | SourceItem::Rollback { .. }) => {}
        }
    }
    Ok(())
}
