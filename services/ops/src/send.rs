//! Building, checking and sending the governance's transactions.
//!
//! Every transaction is **legacy** and paid by the acting key (a member pays its own Squads
//! transactions, as in the Squads app: a separate fee payer would add 96 bytes to each). Before
//! anything is sent, [`Ops::send`]:
//!
//! 1. refuses a transaction above **1,232 bytes** (the packet limit) — the staging commands
//!    switch to the transaction-buffer path before that;
//! 2. **simulates** it and refuses to send one that fails, reporting the program's error by name
//!    ([`TxFailure`]: the program that raised it and the Anchor error name from the logs, so a
//!    Squads `TimeLockNotReleased` and a short.win `TimelockNotElapsed` are told apart even when
//!    short.win's error surfaces through Squads' `vault_transaction_execute`);
//! 3. with `--dry-run`, stops there ([`Ops::dry_run`]): nothing is sent, and a command of
//!    several transactions stops after the first (the later ones depend on it).

use std::cell::RefCell;
use std::io::Write;

use anchor_lang::solana_program::instruction::Instruction;
use anyhow::bail;
use shortwin_services_common::{Chain, TxError, TxOutcome, tx};
use solana_keypair::Keypair;
use solana_message::{Message, VersionedMessage};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;

/// Solana's packet limit (`PACKET_DATA_SIZE`): every transaction must fit.
pub const TX_MAX: usize = 1_232;

/// A transaction the chain refused (or that failed its simulation): returned as the error of
/// [`Ops::send`] so callers and tests can match the program and code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxFailure {
    /// What the tool was sending (e.g. "execute Governance #2").
    pub label: String,
    pub err: TxError,
    /// The program that raised a custom error (the innermost failing program in the logs).
    pub program: Option<Pubkey>,
    /// The Anchor error name from the logs (`Error Code: <name>`), if any.
    pub name: Option<String>,
    /// The last log lines.
    pub logs: Vec<String>,
}

impl TxFailure {
    /// The custom error code, if the failure was one.
    pub fn code(&self) -> Option<u32> {
        self.err.custom_code()
    }

    fn from_outcome(label: &str, out: &TxOutcome) -> Option<Self> {
        let err = out.err.clone()?;
        let failed = |l: &&String| l.contains(" failed: ");
        let program = out
            .logs
            .iter()
            .find(failed)
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|k| k.parse().ok());
        let name = out.logs.iter().find_map(|l| {
            let rest = l.split("Error Code: ").nth(1)?;
            Some(rest.split('.').next()?.trim().to_string())
        });
        let logs = out.logs.iter().rev().take(8).rev().cloned().collect();
        Some(Self {
            label: label.to_string(),
            err,
            program,
            name,
            logs,
        })
    }
}

impl std::fmt::Display for TxFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let who = match self.program {
            Some(p) if p == squads_v4_ix::ID => "Squads".to_string(),
            Some(p) if p == shortwin::ID => "short.win".to_string(),
            Some(p) => p.to_string(),
            None => "the runtime".to_string(),
        };
        write!(f, "{}: refused by {who}: ", self.label)?;
        match (&self.name, self.code()) {
            (Some(n), Some(c)) => write!(f, "{n} ({c})")?,
            (None, Some(c)) => write!(f, "custom error {c}")?,
            _ => write!(f, "{:?}", self.err)?,
        }
        if matches!(self.err, TxError::Dropped(_)) {
            write!(
                f,
                "\nnot confirmed in time: it may still have landed. Check `status` before \
                 rerunning (a staging rerun refuses a duplicate of the newest transaction and \
                 resumes its own transaction buffer)"
            )?;
        }
        if !self.logs.is_empty() {
            write!(f, "\nlast logs:\n  {}", self.logs.join("\n  "))?;
        }
        Ok(())
    }
}

impl std::error::Error for TxFailure {}

/// The legacy wire size of `ixs` paid by `payer` (signatures included), without signing.
pub fn wire_size(payer: &Pubkey, ixs: &[Instruction]) -> usize {
    let msg = Message::new(ixs, Some(payer));
    let n = usize::from(msg.header.num_required_signatures);
    tx::size(&VersionedTransaction {
        signatures: vec![Default::default(); n],
        message: VersionedMessage::Legacy(msg),
    })
}

/// The tool's context: the chain, `--dry-run`, and where the report goes.
pub struct Ops<'a, C: Chain> {
    pub chain: &'a C,
    pub dry_run: bool,
    out: RefCell<&'a mut dyn Write>,
    /// Transactions sent (or simulated, in a dry run) so far: `(label, size)`.
    pub sent: RefCell<Vec<(String, usize)>>,
}

impl<'a, C: Chain> Ops<'a, C> {
    pub fn new(chain: &'a C, dry_run: bool, out: &'a mut dyn Write) -> Self {
        Self {
            chain,
            dry_run,
            out: RefCell::new(out),
            sent: RefCell::new(Vec::new()),
        }
    }

    /// Writes one line of the report.
    pub fn say(&self, line: impl AsRef<str>) {
        // A closed pipe is not worth failing a command that already landed.
        let _ = writeln!(self.out.borrow_mut(), "{}", line.as_ref());
    }

    /// Builds, checks, simulates and (unless dry-running) sends `ixs` (module docs). `payer`
    /// pays and signs; `signers` sign too (only those the message needs). Returns the outcome;
    /// a failure is a [`TxFailure`] error.
    pub async fn send(
        &self,
        label: &str,
        payer: &Keypair,
        ixs: &[Instruction],
        signers: &[&Keypair],
    ) -> anyhow::Result<TxOutcome> {
        let size = wire_size(&payer.pubkey(), ixs);
        if size > TX_MAX {
            bail!("{label}: a {size}-byte transaction (the limit is {TX_MAX})");
        }
        let blockhash = self.chain.latest_blockhash().await?;
        let msg = Message::new_with_blockhash(ixs, Some(&payer.pubkey()), &blockhash);
        let n = usize::from(msg.header.num_required_signatures);
        let required: Vec<Pubkey> = msg.account_keys[..n].to_vec();
        let mut keys: Vec<&Keypair> = vec![payer];
        for k in &required[1..] {
            match signers.iter().find(|s| s.pubkey() == *k) {
                Some(s) => keys.push(s),
                None => bail!("{label}: needs the signature of {k} (give its keypair file)"),
            }
        }
        let signatures = keys
            .iter()
            .map(|k| k.sign_message(&msg.serialize()))
            .collect();
        let vtx = VersionedTransaction {
            signatures,
            message: VersionedMessage::Legacy(msg),
        };
        debug_assert_eq!(tx::size(&vtx), size);

        let sim = self.chain.simulate(&vtx).await?;
        if let Some(f) = TxFailure::from_outcome(label, &sim) {
            return Err(f.into());
        }
        self.sent.borrow_mut().push((label.to_string(), size));
        if self.dry_run {
            self.say(format!(
                "dry-run: {label}: {size} B, {} CU simulated, NOT sent",
                sim.units_consumed
            ));
            return Ok(sim);
        }
        let out = self.chain.send(&vtx).await?;
        if let Some(f) = TxFailure::from_outcome(label, &out) {
            return Err(f.into());
        }
        self.say(format!(
            "{label}: {} ({size} B, {} CU)",
            out.signature, out.units_consumed
        ));
        Ok(out)
    }

    /// In a dry run, the point after a command's first transaction: later transactions depend
    /// on it, so the command stops (returns `true`).
    pub fn stop_dry_run(&self) -> bool {
        if self.dry_run {
            self.say("dry-run: stopping after the first transaction (the next ones depend on it)");
        }
        self.dry_run
    }
}

/// The [`TxFailure`] inside an error of [`Ops::send`], if it is one.
pub fn failure(e: &anyhow::Error) -> Option<&TxFailure> {
    e.downcast_ref::<TxFailure>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_names_the_innermost_program_and_the_anchor_error() {
        let out = TxOutcome {
            err: Some(TxError::Custom {
                index: 0,
                code: 6_021,
            }),
            logs: vec![
                format!("Program {} invoke [1]", squads_v4_ix::ID),
                format!("Program {} invoke [2]", shortwin::ID),
                "Program log: AnchorError occurred. Error Code: TimelockNotElapsed. Error Number: 6021. Error Message: x.".into(),
                format!("Program {} failed: custom program error: 0x1785", shortwin::ID),
                format!("Program {} failed: custom program error: 0x1785", squads_v4_ix::ID),
            ],
            ..TxOutcome::default()
        };
        let f = TxFailure::from_outcome("apply", &out).unwrap();
        assert_eq!(f.program, Some(shortwin::ID));
        assert_eq!(f.name.as_deref(), Some("TimelockNotElapsed"));
        assert_eq!(f.code(), Some(6_021));
        let s = f.to_string();
        assert!(
            s.starts_with("apply: refused by short.win: TimelockNotElapsed (6021)"),
            "{s}"
        );
        assert!(TxFailure::from_outcome("ok", &TxOutcome::default()).is_none());
    }

    #[test]
    fn a_dropped_send_says_to_check_before_rerunning() {
        let out = TxOutcome {
            err: Some(TxError::Dropped("not confirmed before the timeout".into())),
            ..TxOutcome::default()
        };
        let s = TxFailure::from_outcome("stage Emergency #3", &out)
            .unwrap()
            .to_string();
        assert!(
            s.contains("may still have landed") && s.contains("`status`"),
            "{s}"
        );
    }
}
