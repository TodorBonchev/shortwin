//! Decoders of the Squads accounts the governance flows read: `Multisig`, `Proposal`,
//! `TransactionBuffer`, `VaultTransaction` (Anchor accounts: an 8-byte `sha256("account:<Name>")[..8]`
//! discriminator, then Borsh). Each returns `None` on a wrong discriminator or short data.

use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use crate::borsh::Reader;
use crate::disc;
use crate::ix::Member;

/// `Multisig`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Multisig {
    pub create_key: Pubkey,
    /// `Pubkey::default()` for an autonomous multisig.
    pub config_authority: Pubkey,
    pub threshold: u16,
    pub time_lock: u32,
    /// The last transaction index (0: none yet); the next transaction is `transaction_index + 1`.
    pub transaction_index: u64,
    /// Transactions up to this index are stale (set to `transaction_index` by every executed
    /// member, threshold or time-lock change): their proposals can no longer be created,
    /// approved or rejected, and a stale config transaction cannot execute — but an `Approved`
    /// **vault** transaction still can.
    pub stale_transaction_index: u64,
    pub rent_collector: Option<Pubkey>,
    pub bump: u8,
    pub members: Vec<Member>,
}

impl Multisig {
    pub fn decode(data: &[u8]) -> Option<Self> {
        let mut r = Reader::new(data);
        (r.array::<8>()? == disc::ACCOUNT_MULTISIG).then_some(())?;
        let create_key = r.pubkey()?;
        let config_authority = r.pubkey()?;
        let threshold = r.u16()?;
        let time_lock = r.u32()?;
        let transaction_index = r.u64()?;
        let stale_transaction_index = r.u64()?;
        let rent_collector = r.option_pubkey()?;
        let bump = r.u8()?;
        let n = r.len(33)?;
        let members = (0..n)
            .map(|_| {
                Some(Member {
                    key: r.pubkey()?,
                    permissions: r.u8()?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            create_key,
            config_authority,
            threshold,
            time_lock,
            transaction_index,
            stale_transaction_index,
            rent_collector,
            bump,
            members,
        })
    }

    /// `true` if `key` is a member.
    pub fn is_member(&self, key: &Pubkey) -> bool {
        self.members.iter().any(|m| m.key == *key)
    }
}

/// `ProposalStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProposalStatus {
    Draft(i64),
    Active(i64),
    Rejected(i64),
    /// The approval that reached the threshold, at this unix time: the time lock runs from it.
    Approved(i64),
    Executing,
    Executed(i64),
    Cancelled(i64),
}

/// `Proposal`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proposal {
    pub multisig: Pubkey,
    pub transaction_index: u64,
    pub status: ProposalStatus,
    pub bump: u8,
    pub approved: Vec<Pubkey>,
    pub rejected: Vec<Pubkey>,
    pub cancelled: Vec<Pubkey>,
}

impl Proposal {
    pub fn decode(data: &[u8]) -> Option<Self> {
        let mut r = Reader::new(data);
        (r.array::<8>()? == disc::ACCOUNT_PROPOSAL).then_some(())?;
        let multisig = r.pubkey()?;
        let transaction_index = r.u64()?;
        let status = match r.u8()? {
            0 => ProposalStatus::Draft(r.i64()?),
            1 => ProposalStatus::Active(r.i64()?),
            2 => ProposalStatus::Rejected(r.i64()?),
            3 => ProposalStatus::Approved(r.i64()?),
            4 => ProposalStatus::Executing,
            5 => ProposalStatus::Executed(r.i64()?),
            6 => ProposalStatus::Cancelled(r.i64()?),
            _ => return None,
        };
        Some(Self {
            multisig,
            transaction_index,
            status,
            bump: r.u8()?,
            approved: r.pubkeys()?,
            rejected: r.pubkeys()?,
            cancelled: r.pubkeys()?,
        })
    }
}

/// `TransactionBuffer`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransactionBuffer {
    pub multisig: Pubkey,
    pub creator: Pubkey,
    pub buffer_index: u8,
    pub vault_index: u8,
    pub final_buffer_hash: [u8; 32],
    pub final_buffer_size: u16,
    pub buffer: Vec<u8>,
}

impl TransactionBuffer {
    pub fn decode(data: &[u8]) -> Option<Self> {
        let mut r = Reader::new(data);
        (r.array::<8>()? == disc::ACCOUNT_TRANSACTION_BUFFER).then_some(())?;
        Some(Self {
            multisig: r.pubkey()?,
            creator: r.pubkey()?,
            buffer_index: r.u8()?,
            vault_index: r.u8()?,
            final_buffer_hash: r.array()?,
            final_buffer_size: r.u16()?,
            buffer: r.bytes()?.to_vec(),
        })
    }
}

/// `VaultTransaction`: what a `vault_transaction_execute` of it runs. Its `message` is the
/// stored `VaultTransactionMessage` — Borsh with `u32` length prefixes, unlike the compact
/// `TransactionMessage` `vault_transaction_create` takes ([`crate::message::decode`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaultTransaction {
    pub multisig: Pubkey,
    pub creator: Pubkey,
    pub index: u64,
    pub bump: u8,
    pub vault_index: u8,
    pub vault_bump: u8,
    pub ephemeral_signer_bumps: Vec<u8>,
    pub message: crate::message::DecodedMessage,
}

impl VaultTransaction {
    pub fn decode(data: &[u8]) -> Option<Self> {
        let mut r = Reader::new(data);
        (r.array::<8>()? == disc::ACCOUNT_VAULT_TRANSACTION).then_some(())?;
        let multisig = r.pubkey()?;
        let creator = r.pubkey()?;
        let index = r.u64()?;
        let bump = r.u8()?;
        let vault_index = r.u8()?;
        let vault_bump = r.u8()?;
        let ephemeral_signer_bumps = r.bytes()?.to_vec();
        let num_signers = r.u8()?;
        let num_writable_signers = r.u8()?;
        let num_writable_non_signers = r.u8()?;
        let account_keys = r.pubkeys()?;
        let n = r.len(9)?;
        let instructions = (0..n)
            .map(|_| Some((r.u8()?, r.bytes()?.to_vec(), r.bytes()?.to_vec())))
            .collect::<Option<Vec<_>>>()?;
        let n = r.len(40)?;
        let lookups = (0..n)
            .map(|_| Some((r.pubkey()?, r.bytes()?.to_vec(), r.bytes()?.to_vec())))
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            multisig,
            creator,
            index,
            bump,
            vault_index,
            vault_bump,
            ephemeral_signer_bumps,
            message: crate::message::DecodedMessage {
                num_signers,
                num_writable_signers,
                num_writable_non_signers,
                account_keys,
                instructions,
                lookups,
            },
        })
    }

    /// The vault this transaction executes as (`vault_index` of its multisig).
    pub fn vault(&self) -> Pubkey {
        crate::pda::vault(&self.multisig, self.vault_index)
    }

    /// Its ephemeral signers (one per `ephemeral_signer_bumps` entry).
    pub fn ephemeral_signers(&self) -> Vec<Pubkey> {
        let tx = crate::pda::transaction(&self.multisig, self.index);
        (0..self.ephemeral_signer_bumps.len())
            .filter_map(|i| u8::try_from(i).ok())
            .map(|i| crate::pda::ephemeral_signer(&tx, i))
            .collect()
    }

    /// The remaining accounts of this transaction's `vault_transaction_execute`, rebuilt from
    /// the stored message (what [`crate::message::VaultMessage::execute_accounts`] gives for
    /// the compiled one): the static keys with the header's writability, a signer passed as a
    /// signer unless it is the vault or an ephemeral signer (Squads signs for those in its
    /// CPIs). `None` for a message that loads keys from address lookup tables (the caller must
    /// supply the tables' contents).
    pub fn execute_accounts(&self) -> Option<Vec<AccountMeta>> {
        let m = &self.message;
        m.lookups.is_empty().then_some(())?;
        let vault = self.vault();
        let ephemeral = self.ephemeral_signers();
        let (s, ws, wn) = (
            usize::from(m.num_signers),
            usize::from(m.num_writable_signers),
            usize::from(m.num_writable_non_signers),
        );
        Some(
            m.account_keys
                .iter()
                .enumerate()
                .map(|(i, k)| {
                    let signer = i < s && *k != vault && !ephemeral.contains(k);
                    let writable = i < ws || (s..s + wn).contains(&i);
                    AccountMeta {
                        pubkey: *k,
                        is_signer: signer,
                        is_writable: writable,
                    }
                })
                .collect(),
        )
    }

    /// The keys that must sign the `vault_transaction_execute` besides the executing member:
    /// the message's signers that are neither the vault nor an ephemeral signer (e.g. the
    /// current upgrade authority in a `SetAuthorityChecked` handing it to the vault).
    pub fn co_signers(&self) -> Vec<Pubkey> {
        let vault = self.vault();
        let ephemeral = self.ephemeral_signers();
        self.message
            .account_keys
            .iter()
            .take(usize::from(self.message.num_signers))
            .filter(|k| **k != vault && !ephemeral.contains(k))
            .copied()
            .collect()
    }

    /// The instructions a `vault_transaction_execute` of this transaction runs, given that
    /// instruction's remaining accounts (`remaining`: the lookup tables, the static keys, then
    /// the loaded writable and read-only keys — Squads' order,
    /// [`crate::message::VaultMessage::execute_accounts`]). An account is a signer if its
    /// message index is below `num_signers` (the vault and any ephemeral signer: Squads signs
    /// for them in its CPIs) and writable by the message's header (a loaded key by its table
    /// list). `None` if `remaining` does not match the message (wrong count, or a static key
    /// that differs) or an index is out of range.
    pub fn executed_instructions(&self, remaining: &[AccountMeta]) -> Option<Vec<Instruction>> {
        let m = &self.message;
        let tables = m.lookups.len();
        let statics = m.account_keys.len();
        let loaded_w: usize = m.lookups.iter().map(|l| l.1.len()).sum();
        let loaded_r: usize = m.lookups.iter().map(|l| l.2.len()).sum();
        let keys: Vec<Pubkey> = remaining.iter().skip(tables).map(|a| a.pubkey).collect();
        (keys.len() == statics + loaded_w + loaded_r).then_some(())?;
        (keys[..statics] == m.account_keys[..]).then_some(())?;
        let (s, ws, wn) = (
            usize::from(m.num_signers),
            usize::from(m.num_writable_signers),
            usize::from(m.num_writable_non_signers),
        );
        let meta = |i: usize| -> Option<AccountMeta> {
            let pubkey = *keys.get(i)?;
            let is_signer = i < s;
            let is_writable = if i < statics {
                i < ws || (s..s + wn).contains(&i)
            } else {
                i < statics + loaded_w
            };
            Some(AccountMeta {
                pubkey,
                is_signer,
                is_writable,
            })
        };
        m.instructions
            .iter()
            .map(|(program, accounts, data)| {
                Some(Instruction {
                    program_id: *keys.get(usize::from(*program))?,
                    accounts: accounts
                        .iter()
                        .map(|i| meta(usize::from(*i)))
                        .collect::<Option<Vec<_>>>()?,
                    data: data.clone(),
                })
            })
            .collect()
    }
}
