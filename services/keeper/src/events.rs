//! Decoding short.win events from a [`TxOutcome`] (simulated or landed; ARCHITECTURE
//! §3.10): `emit_cpi!` events from the inner instructions of short.win's self-CPI
//! (`EVENT_IX_TAG_LE ‖ discriminator ‖ Borsh`), and the `emit!` log events (`CoverageGap`,
//! an exit's `MintSafetyTripped`) from `Program data:` log lines. The pool-selection events
//! (`PoolProposed`, `PoolSwitched`, `PoolSwitchCancelled`; M4) are reported as [`PoolEvent`]s.

use anchor_lang::__private::base64::{self, Engine};
use anchor_lang::prelude::Pubkey;
use anchor_lang::{AnchorDeserialize, Discriminator, Event};
use shortwin_services_common::{TxError, TxOutcome};

fn decode<T: Event + Discriminator + AnchorDeserialize>(payload: &[u8]) -> Option<T> {
    let body = payload.strip_prefix(T::DISCRIMINATOR)?;
    T::try_from_slice(body).ok()
}

/// Every `emit_cpi!` event of type `T` of top-level instruction `ix`.
pub fn of_ix<T: Event + Discriminator + AnchorDeserialize>(out: &TxOutcome, ix: usize) -> Vec<T> {
    let tag = anchor_lang::event::EVENT_IX_TAG_LE;
    out.inner
        .get(ix)
        .map(|inner| {
            inner
                .iter()
                .filter(|i| i.program_id == shortwin::ID)
                .filter_map(|i| i.data.strip_prefix(tag))
                .filter_map(decode::<T>)
                .collect()
        })
        .unwrap_or_default()
}

/// Every `emit_cpi!` event of type `T` in the transaction.
pub fn of<T: Event + Discriminator + AnchorDeserialize>(out: &TxOutcome) -> Vec<T> {
    (0..out.inner.len())
        .flat_map(|i| of_ix::<T>(out, i))
        .collect()
}

/// Every `emit!` log event of type `T` in the transaction's logs.
pub fn logged<T: Event + Discriminator + AnchorDeserialize>(out: &TxOutcome) -> Vec<T> {
    let engine = base64::engine::general_purpose::STANDARD;
    out.logs
        .iter()
        .filter_map(|line| line.strip_prefix("Program data: "))
        .filter_map(|b64| engine.decode(b64.trim()).ok())
        .filter_map(|p| decode::<T>(&p))
        .collect()
}

/// The program whose failure ended the transaction: the first `Program <id> failed` line
/// (the innermost failing program logs first).
pub fn failing_program(out: &TxOutcome) -> Option<anchor_lang::prelude::Pubkey> {
    out.logs.iter().find_map(|l| {
        let rest = l.strip_prefix("Program ")?;
        let (id, tail) = rest.split_once(' ')?;
        tail.starts_with("failed")
            .then(|| id.parse().ok())
            .flatten()
    })
}

/// AIR-18 (KI-63; leads.md L-2): whether the transaction failed because the loader would not run
/// one of its programs — `InstructionError::UnsupportedProgramId` ("Program is not deployed").
/// The loader answers so for a program deployed, upgraded or extended in the current slot until
/// the next slot makes the new deployment visible (Agave's `DelayVisibility`): its `ExtendProgram`
/// is permissionless (once a slot per program), so anyone can do this to short.win — every
/// instruction fails — or to a venue — every exit buying on it fails — for the rest of a slot.
/// But it gives the same log and error for a program that does not come back by itself: closed,
/// failing verification (an ELF a feature gate retired, as SBPFv0's would every venue fixture),
/// or not cached. [`failing_program`] names the program (the innermost failing one: a venue behind
/// short.win's CPI); [`deployed_in_refused_slot`] tells the transient case by its ProgramData.
pub fn not_deployed(out: &TxOutcome) -> bool {
    matches!(
        &out.err,
        Some(TxError::Instruction { detail, .. }) if detail == "UnsupportedProgramId"
    )
}

/// AIR-18: slots a deploy may precede the slot a refusal is dated by and still be judged by
/// [`deployed_in_refused_slot`]'s bound below. A refusal is dated by its own slot (a simulation's
/// bank, a failed transaction's block), which is the deploy's; when the backend does not say, by
/// the clock read of the tick that sent it, which the nodes of an RPC pool can serve a slot or
/// two apart (all at `confirmed`).
pub const DEPLOY_SLOT_SLACK: u64 = 2;

/// AIR-18: whether a refusal of [`not_deployed`] dated by slot `refused_slot` can be the loader's
/// one-slot `DelayVisibility`, given the deploy slot the refused program's ProgramData header
/// shows as of that slot or later (`None`: no ProgramData — closed, or a program not under the
/// upgradeable loader). That refusal comes in the slot of the deploy, upgrade or extension
/// (within [`DEPLOY_SLOT_SLACK`] of the slot it is dated by); a deploy slot only grows. An older
/// deploy slot means the program does not come back by itself (failing verification, a stale
/// cache entry): the keeper handles it as any refusal, alerted.
pub fn deployed_in_refused_slot(deploy_slot: Option<u64>, refused_slot: u64) -> bool {
    deploy_slot.is_some_and(|d| d.saturating_add(DEPLOY_SLOT_SLACK) >= refused_slot)
}

/// Which pool-selection event (ARCH §3.10; M4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PoolEventKind {
    /// `PoolProposed`: a fast-path collection window opened or improved.
    Proposed,
    /// `PoolSwitched`: an adoption (listing, listing window, zero-position, fast path).
    Switched,
    /// `PoolSwitchCancelled`: a window closed without an adoption.
    Cancelled,
}

/// A pool-selection event (`PoolProposed`, `PoolSwitched`, `PoolSwitchCancelled` share their
/// fields; the Anchor event types derive neither `Debug` nor `Clone`, so the keeper reports
/// this copy).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolEvent {
    pub what: PoolEventKind,
    pub market: Pubkey,
    pub slot: u64,
    pub ts: i64,
    /// The candidate pool (`Switched`: the pool now adopted).
    pub candidate: Pubkey,
    /// `shortwin::events::POOL_KIND_*` ([`pool_kind_name`]).
    pub kind: u8,
    /// The candidate's locked depth `D` (`Cancelled`: the best one seen).
    pub d_cand: u128,
    /// The incumbent's locked depth `pool_d` (0 for a list-only market).
    pub d_inc: u128,
    /// `Proposed` / `Cancelled`: the end of the collection window; `Switched`: the end of the
    /// listing window after the adoption (opens resume then).
    pub window_ends: i64,
}

macro_rules! pool_event_of {
    ($what:expr, $e:expr) => {
        PoolEvent {
            what: $what,
            market: $e.market,
            slot: $e.slot,
            ts: $e.ts,
            candidate: $e.candidate,
            kind: $e.kind,
            d_cand: $e.d_cand,
            d_inc: $e.d_inc,
            window_ends: $e.window_ends,
        }
    };
}

/// Every pool-selection event of the transaction, in instruction order.
pub fn pool_events(out: &TxOutcome) -> Vec<PoolEvent> {
    use shortwin::events::{PoolProposed, PoolSwitchCancelled, PoolSwitched};
    let mut v = Vec::new();
    for ix in 0..out.inner.len() {
        v.extend(
            of_ix::<PoolProposed>(out, ix)
                .into_iter()
                .map(|e| pool_event_of!(PoolEventKind::Proposed, e)),
        );
        v.extend(
            of_ix::<PoolSwitched>(out, ix)
                .into_iter()
                .map(|e| pool_event_of!(PoolEventKind::Switched, e)),
        );
        v.extend(
            of_ix::<PoolSwitchCancelled>(out, ix)
                .into_iter()
                .map(|e| pool_event_of!(PoolEventKind::Cancelled, e)),
        );
    }
    v
}

/// The name of a `POOL_KIND_*` value.
pub fn pool_kind_name(kind: u8) -> &'static str {
    use shortwin::events::{
        POOL_KIND_FAST, POOL_KIND_LISTING, POOL_KIND_LISTING_WINDOW, POOL_KIND_ZERO_POSITION,
    };
    match kind {
        POOL_KIND_LISTING => "listing",
        POOL_KIND_LISTING_WINDOW => "listing window",
        POOL_KIND_ZERO_POSITION => "zero-position",
        POOL_KIND_FAST => "fast path",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::{AnchorSerialize, Discriminator};
    use shortwin::events::{POOL_KIND_FAST, PoolSwitched};
    use shortwin_services_common::InnerIx;

    /// AIR-18: the loader's refusal of a program deployed in this slot — at the top level, or
    /// behind short.win's CPI (the venue, the innermost failing program, is named) — is told
    /// from every other failure.
    #[test]
    fn a_program_not_deployed_in_this_slot_is_told_and_named() {
        let venue = Pubkey::new_unique();
        let refused = |index: u8, logs: Vec<String>| TxOutcome {
            err: Some(TxError::Instruction {
                index,
                detail: "UnsupportedProgramId".into(),
            }),
            logs,
            ..TxOutcome::default()
        };
        let cpi = refused(
            1,
            vec![
                format!("Program {} invoke [1]", shortwin::ID),
                format!("Program {venue} invoke [2]"),
                "Program is not deployed".into(),
                format!("Program {venue} failed: Unsupported program id"),
                format!(
                    "Program {} consumed 41200 of 200000 compute units",
                    shortwin::ID
                ),
                format!("Program {} failed: Unsupported program id", shortwin::ID),
            ],
        );
        assert!(not_deployed(&cpi));
        assert_eq!(failing_program(&cpi), Some(venue));
        let top = refused(
            0,
            vec![
                format!("Program {} invoke [1]", shortwin::ID),
                "Program is not deployed".into(),
                format!("Program {} failed: Unsupported program id", shortwin::ID),
            ],
        );
        assert!(not_deployed(&top));
        assert_eq!(failing_program(&top), Some(shortwin::ID));
        assert_eq!(failing_program(&refused(0, Vec::new())), None);
        for err in [
            TxError::Custom {
                index: 0,
                code: 6_071,
            },
            TxError::Instruction {
                index: 0,
                detail: "InvalidAccountData".into(),
            },
            TxError::Execution("InsufficientFundsForFee".into()),
            TxError::Rejected("BlockhashNotFound".into()),
            TxError::Dropped("not confirmed".into()),
        ] {
            let out = TxOutcome {
                err: Some(err),
                ..TxOutcome::default()
            };
            assert!(!not_deployed(&out), "{:?}", out.err);
        }
        assert!(!not_deployed(&TxOutcome::default()), "a success");
    }

    /// AIR-18: a refusal is the one-slot `DelayVisibility` only when the refused program's
    /// ProgramData shows a deploy in the refused slot or later (within the slack); an older deploy
    /// (a program failing verification) or none (closed, not upgradeable) is a refusal that stays.
    #[test]
    fn only_a_deploy_in_the_refused_slot_makes_a_refusal_transient() {
        let tick = 452_376_460;
        for d in [tick, tick + 1, tick + 7, tick - DEPLOY_SLOT_SLACK] {
            assert!(deployed_in_refused_slot(Some(d), tick), "{d}");
        }
        for d in [tick - DEPLOY_SLOT_SLACK - 1, tick - 1_000, 0] {
            assert!(!deployed_in_refused_slot(Some(d), tick), "{d}");
        }
        assert!(!deployed_in_refused_slot(None, tick), "no ProgramData");
        assert!(deployed_in_refused_slot(Some(0), 0));
        assert!(deployed_in_refused_slot(Some(u64::MAX), tick));
    }

    /// An `emit_cpi!` payload decodes into the keeper's copy with every field.
    #[test]
    fn pool_events_decode_from_the_self_cpi() {
        let e = PoolSwitched {
            market: Pubkey::new_unique(),
            slot: 7,
            ts: 1_700_000_000,
            candidate: Pubkey::new_unique(),
            kind: POOL_KIND_FAST,
            d_cand: 12_345,
            d_inc: 20_000,
            window_ends: 1_700_003_600,
        };
        let mut data = anchor_lang::event::EVENT_IX_TAG_LE.to_vec();
        data.extend_from_slice(PoolSwitched::DISCRIMINATOR);
        e.serialize(&mut data).unwrap();
        let out = TxOutcome {
            inner: vec![vec![
                InnerIx {
                    program_id: Pubkey::new_unique(),
                    data: data.clone(),
                },
                InnerIx {
                    program_id: shortwin::ID,
                    data,
                },
            ]],
            ..TxOutcome::default()
        };
        let got = pool_events(&out);
        assert_eq!(
            got,
            vec![PoolEvent {
                what: PoolEventKind::Switched,
                market: e.market,
                slot: 7,
                ts: e.ts,
                candidate: e.candidate,
                kind: POOL_KIND_FAST,
                d_cand: 12_345,
                d_inc: 20_000,
                window_ends: e.window_ends,
            }],
            "only short.win's own inner instruction counts"
        );
        assert_eq!(pool_kind_name(POOL_KIND_FAST), "fast path");
    }
}
