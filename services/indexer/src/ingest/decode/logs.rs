//! The log invocation-stack parser (INDEXER.md §5.2). Owner: INGEST.
//!
//! Tracks `Program <id> invoke [n]` (push; n = 1 starts the next top-level instruction),
//! `Program <id> success` / `Program <id> failed: …` (pop), and attributes each
//! `Program data: <base64>` / `Program log: ray_log: <base64>` line to the program on top of the
//! stack, with its top-level instruction index and its line index (`ev_seq`). "Log truncated"
//! ends the parse (`ingest_txs.logs_truncated`).
//!
//! Every frame is also matched to its node of the transaction's invocation tree (the top-level
//! instruction, or the next inner instruction with the same program and stack height, in
//! execution order), so a log line knows the invocation that printed it and the short.win
//! invocation below it (a venue trade's `shortwin_invocation`). Any program can print
//! "Program data:" (`sol_log_data`); only the attribution by the stack decides whose event a line
//! is (§5.1, §10.3 "forged logs"). A `msg!("Program data: …")` is a "Program log: …" line and is
//! never read as data.

use base64::Engine;
use solana_pubkey::Pubkey;

use crate::ingest::raw::RawTx;

/// What a log line carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogPayload {
    /// `Program data: <base64>` (`sol_log_data`, Anchor's `emit!`).
    Data(Vec<u8>),
    /// `Program log: ray_log: <base64>` (Raydium AMM v4's swap record).
    RayLog(Vec<u8>),
}

/// A node of a top-level instruction's invocation tree: `None` = the top-level instruction,
/// `Some(j)` = its inner instruction `j`.
pub type Node = Option<usize>;

/// One attributed log line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    /// Top-level instruction index.
    pub ix_index: usize,
    /// Index of the line in the transaction's logs (`ev_seq` of a log event).
    pub line: usize,
    /// The program on top of the stack when the line was printed.
    pub program: Pubkey,
    /// The invocation-tree node of that frame, when matched.
    pub node: Option<Node>,
    /// The stack below the top (outermost first): `(program, node)`.
    pub below: Vec<(Pubkey, Option<Node>)>,
    pub payload: LogPayload,
}

/// The parsed log stream of one transaction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedLogs {
    pub entries: Vec<LogEntry>,
    /// "Log truncated" was seen (log events after it are lost; §5.1).
    pub truncated: bool,
}

fn parse_invoke(line: &str) -> Option<(Pubkey, usize)> {
    let rest = line.strip_prefix("Program ")?;
    let (id, rest) = rest.split_once(' ')?;
    let depth = rest.strip_prefix("invoke [")?.strip_suffix(']')?;
    Some((id.parse().ok()?, depth.parse().ok()?))
}

fn parse_exit(line: &str) -> Option<Pubkey> {
    let rest = line.strip_prefix("Program ")?;
    let (id, rest) = rest.split_once(' ')?;
    (rest == "success" || rest.starts_with("failed")).then_some(())?;
    id.parse().ok()
}

/// Parses `raw.logs` against `raw`'s invocation tree.
pub fn parse(raw: &RawTx) -> ParsedLogs {
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut out = ParsedLogs::default();
    // The current top-level instruction (None before the first `invoke [1]`).
    let mut top: Option<usize> = None;
    // Next unmatched inner index of the current top-level instruction.
    let mut cursor = 0usize;
    let mut stack: Vec<(Pubkey, Option<Node>)> = Vec::new();
    for (line_no, line) in raw.logs.iter().enumerate() {
        if line == "Log truncated" {
            out.truncated = true;
            break;
        }
        if let Some((id, depth)) = parse_invoke(line) {
            if depth == 1 {
                // The next top-level instruction of this program (instructions that log
                // nothing, e.g. precompiles, are skipped).
                let from = top.map_or(0, |t| t + 1);
                let k = (from..raw.instructions.len())
                    .find(|&k| raw.instructions[k].ix.program_id == id)
                    .unwrap_or(from);
                top = Some(k);
                cursor = 0;
                stack.clear();
                let node = (k < raw.instructions.len()).then_some(None);
                stack.push((id, node));
            } else {
                // Resynchronise on the declared depth.
                stack.truncate(depth.saturating_sub(1));
                let node = top.and_then(|k| {
                    let inner = &raw.instructions.get(k)?.inner;
                    let j = (cursor..inner.len()).find(|&j| {
                        inner[j].program_id == id
                            && usize::try_from(inner[j].stack_height).ok() == Some(depth)
                    })?;
                    cursor = j + 1;
                    Some(Some(j))
                });
                stack.push((id, node));
            }
            continue;
        }
        if let Some(id) = parse_exit(line) {
            if stack.last().is_some_and(|(p, _)| *p == id) {
                stack.pop();
            }
            continue;
        }
        let payload = if let Some(d) = line.strip_prefix("Program data: ") {
            match b64.decode(d.trim()) {
                Ok(b) => LogPayload::Data(b),
                Err(_) => continue,
            }
        } else if let Some(d) = line.strip_prefix("Program log: ray_log: ") {
            match b64.decode(d.trim()) {
                Ok(b) => LogPayload::RayLog(b),
                Err(_) => continue,
            }
        } else {
            continue;
        };
        let (Some(k), Some((program, node))) = (top, stack.last().cloned()) else {
            continue;
        };
        out.entries.push(LogEntry {
            ix_index: k,
            line: line_no,
            program,
            node,
            below: stack[..stack.len() - 1].to_vec(),
            payload,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use solana_signature::Signature;

    use super::*;
    use crate::ingest::raw::{Commitment, RawIx, RawTopIx};

    fn ix(p: Pubkey, h: u32) -> RawIx {
        RawIx {
            program_id: p,
            accounts: vec![],
            data: vec![],
            stack_height: h,
        }
    }

    fn tx(instructions: Vec<RawTopIx>, logs: &[String]) -> RawTx {
        RawTx {
            signature: Signature::default(),
            slot: 1,
            tx_index: 0,
            block_time: None,
            commitment: Commitment::Finalized,
            fee_payer: Pubkey::default(),
            fee: 0,
            compute_units: None,
            err: None,
            account_keys: vec![],
            instructions,
            logs: logs.to_vec(),
        }
    }

    #[test]
    fn lines_are_attributed_to_the_program_on_top_and_matched_to_nodes() {
        let a = Pubkey::new_from_array([1; 32]);
        let b = Pubkey::new_from_array([2; 32]);
        let c = Pubkey::new_from_array([3; 32]);
        let t = tx(
            vec![
                RawTopIx {
                    ix: ix(a, 1),
                    inner: vec![ix(b, 2), ix(c, 3), ix(b, 2)],
                },
                RawTopIx {
                    ix: ix(c, 1),
                    inner: vec![],
                },
            ],
            &[
                format!("Program {a} invoke [1]"),
                format!("Program {b} invoke [2]"),
                format!("Program {c} invoke [3]"),
                "Program data: AQI=".into(),
                format!("Program {c} success"),
                "Program log: Program data: AQI=".into(),
                format!("Program {b} success"),
                format!("Program {b} invoke [2]"),
                "Program log: ray_log: Aw==".into(),
                format!("Program {b} success"),
                "Program data: BQ==".into(),
                format!("Program {a} success"),
                format!("Program {c} invoke [1]"),
                "Program data: Bg==".into(),
                "Log truncated".into(),
                "Program data: Bw==".into(),
            ],
        );
        let p = parse(&t);
        assert!(p.truncated);
        assert_eq!(p.entries.len(), 4);
        // c at height 3 (inner 1), with a and b (inner 0) below it.
        assert_eq!(p.entries[0].program, c);
        assert_eq!(p.entries[0].node, Some(Some(1)));
        assert_eq!(
            p.entries[0].below,
            vec![(a, Some(None)), (b, Some(Some(0)))]
        );
        assert_eq!(p.entries[0].payload, LogPayload::Data(vec![1, 2]));
        // `msg!("Program data: …")` is a "Program log:" line: never data. The second `b` frame
        // is inner 2.
        assert_eq!(p.entries[1].program, b);
        assert_eq!(p.entries[1].node, Some(Some(2)));
        assert_eq!(p.entries[1].payload, LogPayload::RayLog(vec![3]));
        assert_eq!((p.entries[2].program, p.entries[2].line), (a, 10));
        assert_eq!(p.entries[3].ix_index, 1);
        assert_eq!(p.entries[3].node, Some(None));
    }

    #[test]
    fn a_failed_frame_pops_and_unlogged_top_level_instructions_are_skipped() {
        let a = Pubkey::new_from_array([1; 32]);
        let pre = Pubkey::new_from_array([4; 32]);
        let t = tx(
            vec![
                RawTopIx {
                    ix: ix(pre, 1),
                    inner: vec![],
                },
                RawTopIx {
                    ix: ix(a, 1),
                    inner: vec![ix(a, 2)],
                },
            ],
            &[
                format!("Program {a} invoke [1]"),
                format!("Program {a} invoke [2]"),
                format!("Program {a} failed: custom program error: 0x1"),
                "Program data: AQ==".into(),
            ],
        );
        let p = parse(&t);
        assert_eq!(p.entries.len(), 1);
        assert_eq!(p.entries[0].ix_index, 1);
        assert_eq!(p.entries[0].node, Some(None));
        assert!(p.entries[0].below.is_empty());
    }
}
