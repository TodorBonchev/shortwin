//! Instruction builders, in the vendored IDL's account order and argument layout (pinned by
//! `tests/idl.rs`). Optional accounts not passed are the program id (Anchor's "none").

use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

use crate::borsh::Writer;
use crate::message::VaultMessage;
use crate::{ID, SYSTEM_PROGRAM, disc, pda};

/// A member's permission bits (`Permissions.mask`).
pub mod permission {
    pub const INITIATE: u8 = 1;
    pub const VOTE: u8 = 2;
    pub const EXECUTE: u8 = 4;
    /// Initiate | Vote | Execute.
    pub const ALL: u8 = INITIATE | VOTE | EXECUTE;
}

/// `Member { key, permissions: Permissions { mask } }`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Member {
    pub key: Pubkey,
    pub permissions: u8,
}

impl Member {
    /// A member with every permission.
    pub fn full(key: Pubkey) -> Self {
        Self {
            key,
            permissions: permission::ALL,
        }
    }

    fn write(&self, w: &mut Writer) {
        w.pubkey(&self.key).u8(self.permissions);
    }
}

/// `MultisigCreateArgsV2`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultisigCreateArgs {
    /// `None`: an autonomous multisig (every config change is itself a voted, time-locked
    /// config transaction).
    pub config_authority: Option<Pubkey>,
    pub threshold: u16,
    pub members: Vec<Member>,
    /// Seconds between a proposal's approval and its execution (at most 90 days).
    pub time_lock: u32,
    pub rent_collector: Option<Pubkey>,
    pub memo: Option<String>,
}

/// `multisig_create_v2(args)`: creates the multisig at `pda::multisig(create_key)`, paying
/// `ProgramConfig.multisig_creation_fee` to `treasury` (which must equal the config's).
pub fn multisig_create_v2(
    treasury: &Pubkey,
    create_key: &Pubkey,
    creator: &Pubkey,
    args: &MultisigCreateArgs,
) -> Instruction {
    let mut w = Writer::with(&disc::MULTISIG_CREATE_V2);
    w.option_pubkey(args.config_authority.as_ref())
        .u16(args.threshold)
        .len(args.members.len());
    for m in &args.members {
        m.write(&mut w);
    }
    w.u32(args.time_lock)
        .option_pubkey(args.rent_collector.as_ref())
        .option_string(args.memo.as_deref());
    Instruction {
        program_id: ID,
        accounts: vec![
            AccountMeta::new_readonly(pda::program_config(), false),
            AccountMeta::new(*treasury, false),
            AccountMeta::new(pda::multisig(create_key), false),
            AccountMeta::new_readonly(*create_key, true),
            AccountMeta::new(*creator, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
        ],
        data: w.into_inner(),
    }
}

fn vault_transaction_create_args(
    w: &mut Writer,
    vault_index: u8,
    ephemeral_signers: u8,
    message: &[u8],
    memo: Option<&str>,
) {
    w.u8(vault_index)
        .u8(ephemeral_signers)
        .bytes(message)
        .option_string(memo);
}

fn vault_transaction_create_accounts(
    multisig: &Pubkey,
    index: u64,
    creator: &Pubkey,
    rent_payer: &Pubkey,
) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(*multisig, false),
        AccountMeta::new(pda::transaction(multisig, index), false),
        AccountMeta::new_readonly(*creator, true),
        AccountMeta::new(*rent_payer, true),
        AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
    ]
}

/// `vault_transaction_create(vault_index, ephemeral_signers, message, memo)`: transaction
/// `index` (the multisig's `transaction_index + 1`) of `multisig`, created by member
/// `creator` (Initiate).
// The IDL's accounts and arguments, one parameter each.
#[allow(clippy::too_many_arguments)]
pub fn vault_transaction_create(
    multisig: &Pubkey,
    index: u64,
    creator: &Pubkey,
    rent_payer: &Pubkey,
    vault_index: u8,
    ephemeral_signers: u8,
    message: &[u8],
    memo: Option<&str>,
) -> Instruction {
    let mut w = Writer::with(&disc::VAULT_TRANSACTION_CREATE);
    vault_transaction_create_args(&mut w, vault_index, ephemeral_signers, message, memo);
    Instruction {
        program_id: ID,
        accounts: vault_transaction_create_accounts(multisig, index, creator, rent_payer),
        data: w.into_inner(),
    }
}

/// The "empty" message `vault_transaction_create_from_buffer` requires in its arguments
/// (`[0; 6]`: no keys, instructions or lookups).
pub const EMPTY_MESSAGE: [u8; 6] = [0; 6];

/// `vault_transaction_create_from_buffer(..)`: transaction `index` from `creator`'s complete
/// buffer `buffer_index` (hash and size checked; the buffer is closed to `creator`).
// The IDL's accounts and arguments, one parameter each.
#[allow(clippy::too_many_arguments)]
pub fn vault_transaction_create_from_buffer(
    multisig: &Pubkey,
    index: u64,
    creator: &Pubkey,
    rent_payer: &Pubkey,
    buffer_index: u8,
    vault_index: u8,
    ephemeral_signers: u8,
    memo: Option<&str>,
) -> Instruction {
    let mut w = Writer::with(&disc::VAULT_TRANSACTION_CREATE_FROM_BUFFER);
    vault_transaction_create_args(&mut w, vault_index, ephemeral_signers, &EMPTY_MESSAGE, memo);
    let mut accounts = vault_transaction_create_accounts(multisig, index, creator, rent_payer);
    accounts.push(AccountMeta::new(
        pda::transaction_buffer(multisig, creator, buffer_index),
        false,
    ));
    accounts.push(AccountMeta::new(*creator, true));
    Instruction {
        program_id: ID,
        accounts,
        data: w.into_inner(),
    }
}

/// `transaction_buffer_create(buffer_index, vault_index, final_buffer_hash, final_buffer_size,
/// buffer)`: `creator`'s buffer `buffer_index`, starting with `chunk`. `final_hash` is the
/// sha256 of the whole message (`final_size` bytes, at most 10,128).
// The IDL's accounts and arguments, one parameter each.
#[allow(clippy::too_many_arguments)]
pub fn transaction_buffer_create(
    multisig: &Pubkey,
    creator: &Pubkey,
    rent_payer: &Pubkey,
    buffer_index: u8,
    vault_index: u8,
    final_hash: [u8; 32],
    final_size: u16,
    chunk: &[u8],
) -> Instruction {
    let mut w = Writer::with(&disc::TRANSACTION_BUFFER_CREATE);
    w.u8(buffer_index)
        .u8(vault_index)
        .raw(&final_hash)
        .u16(final_size)
        .bytes(chunk);
    Instruction {
        program_id: ID,
        accounts: vec![
            AccountMeta::new_readonly(*multisig, false),
            AccountMeta::new(
                pda::transaction_buffer(multisig, creator, buffer_index),
                false,
            ),
            AccountMeta::new_readonly(*creator, true),
            AccountMeta::new(*rent_payer, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
        ],
        data: w.into_inner(),
    }
}

/// `transaction_buffer_extend(buffer)`: appends `chunk` to `creator`'s buffer.
pub fn transaction_buffer_extend(
    multisig: &Pubkey,
    creator: &Pubkey,
    buffer_index: u8,
    chunk: &[u8],
) -> Instruction {
    let mut w = Writer::with(&disc::TRANSACTION_BUFFER_EXTEND);
    w.bytes(chunk);
    Instruction {
        program_id: ID,
        accounts: vec![
            AccountMeta::new_readonly(*multisig, false),
            AccountMeta::new(
                pda::transaction_buffer(multisig, creator, buffer_index),
                false,
            ),
            AccountMeta::new_readonly(*creator, true),
        ],
        data: w.into_inner(),
    }
}

/// `transaction_buffer_close()`: closes `creator`'s buffer to `creator`. The IDL (and this
/// builder) mark `creator` read-only although the rent is credited to it: the transaction must
/// make it writable otherwise, e.g. as its fee payer.
pub fn transaction_buffer_close(
    multisig: &Pubkey,
    creator: &Pubkey,
    buffer_index: u8,
) -> Instruction {
    Instruction {
        program_id: ID,
        accounts: vec![
            AccountMeta::new_readonly(*multisig, false),
            AccountMeta::new(
                pda::transaction_buffer(multisig, creator, buffer_index),
                false,
            ),
            AccountMeta::new_readonly(*creator, true),
        ],
        data: disc::TRANSACTION_BUFFER_CLOSE.to_vec(),
    }
}

/// `proposal_create(transaction_index, draft)`: the proposal of transaction `index`; `draft =
/// false` makes it `Active` at once (votes can follow in the same transaction).
pub fn proposal_create(
    multisig: &Pubkey,
    index: u64,
    creator: &Pubkey,
    rent_payer: &Pubkey,
    draft: bool,
) -> Instruction {
    let mut w = Writer::with(&disc::PROPOSAL_CREATE);
    w.u64(index).bool(draft);
    Instruction {
        program_id: ID,
        accounts: vec![
            AccountMeta::new_readonly(*multisig, false),
            AccountMeta::new(pda::proposal(multisig, index), false),
            AccountMeta::new_readonly(*creator, true),
            AccountMeta::new(*rent_payer, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
        ],
        data: w.into_inner(),
    }
}

fn vote(
    d: [u8; 8],
    multisig: &Pubkey,
    index: u64,
    member: &Pubkey,
    memo: Option<&str>,
) -> Instruction {
    let mut w = Writer::with(&d);
    w.option_string(memo);
    Instruction {
        program_id: ID,
        accounts: vec![
            AccountMeta::new_readonly(*multisig, false),
            AccountMeta::new(*member, true),
            AccountMeta::new(pda::proposal(multisig, index), false),
        ],
        data: w.into_inner(),
    }
}

/// `proposal_approve(memo)` by `member` (Vote) on an `Active` proposal; the approval that
/// reaches the threshold makes it `Approved { timestamp: now }` — the time lock runs from there.
pub fn proposal_approve(multisig: &Pubkey, index: u64, member: &Pubkey) -> Instruction {
    vote(disc::PROPOSAL_APPROVE, multisig, index, member, None)
}

/// `proposal_reject(memo)` by `member` (Vote) on an `Active` proposal.
pub fn proposal_reject(multisig: &Pubkey, index: u64, member: &Pubkey) -> Instruction {
    vote(disc::PROPOSAL_REJECT, multisig, index, member, None)
}

/// `proposal_cancel(memo)` by `member` (Vote) on an `Approved` proposal; `threshold` cancel
/// votes make it `Cancelled`. Superseded by [`proposal_cancel_v2`], which reallocates the
/// proposal when the multisig has grown since it was created.
pub fn proposal_cancel(multisig: &Pubkey, index: u64, member: &Pubkey) -> Instruction {
    vote(disc::PROPOSAL_CANCEL, multisig, index, member, None)
}

/// `proposal_cancel_v2(memo)`: [`proposal_cancel`] with the system program for the realloc.
pub fn proposal_cancel_v2(multisig: &Pubkey, index: u64, member: &Pubkey) -> Instruction {
    let mut ix = vote(disc::PROPOSAL_CANCEL_V2, multisig, index, member, None);
    ix.accounts
        .push(AccountMeta::new_readonly(SYSTEM_PROGRAM, false));
    ix
}

/// `vault_transaction_execute()` of transaction `index` by `member` (Execute), with `message`'s
/// remaining accounts ([`VaultMessage::execute_accounts`]; no ephemeral signers).
pub fn vault_transaction_execute(
    multisig: &Pubkey,
    index: u64,
    member: &Pubkey,
    message: &VaultMessage,
) -> Instruction {
    vault_transaction_execute_with(multisig, index, member, message.execute_accounts(&[]))
}

/// `vault_transaction_execute()` of transaction `index` by `member` with explicit remaining
/// accounts — e.g. [`crate::state::VaultTransaction::execute_accounts`] of the stored
/// transaction, when the compiled message is not at hand (the ops tool executes what it reads).
pub fn vault_transaction_execute_with(
    multisig: &Pubkey,
    index: u64,
    member: &Pubkey,
    remaining: Vec<AccountMeta>,
) -> Instruction {
    let mut accounts = vec![
        AccountMeta::new_readonly(*multisig, false),
        AccountMeta::new(pda::proposal(multisig, index), false),
        AccountMeta::new_readonly(pda::transaction(multisig, index), false),
        AccountMeta::new_readonly(*member, true),
    ];
    accounts.extend(remaining);
    Instruction {
        program_id: ID,
        accounts,
        data: disc::VAULT_TRANSACTION_EXECUTE.to_vec(),
    }
}

/// How many named accounts `vault_transaction_execute` has before its remaining accounts.
pub const VAULT_TRANSACTION_EXECUTE_NAMED: usize = 4;

/// `Period` of a spending limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Period {
    OneTime,
    Day,
    Week,
    Month,
}

/// `ConfigAction` (variant order as in the IDL).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigAction {
    AddMember(Member),
    RemoveMember(Pubkey),
    ChangeThreshold(u16),
    SetTimeLock(u32),
    AddSpendingLimit {
        create_key: Pubkey,
        vault_index: u8,
        mint: Pubkey,
        amount: u64,
        period: Period,
        members: Vec<Pubkey>,
        destinations: Vec<Pubkey>,
    },
    RemoveSpendingLimit(Pubkey),
    SetRentCollector(Option<Pubkey>),
}

impl ConfigAction {
    fn write(&self, w: &mut Writer) {
        match self {
            Self::AddMember(m) => {
                w.u8(0);
                m.write(w);
            }
            Self::RemoveMember(k) => {
                w.u8(1).pubkey(k);
            }
            Self::ChangeThreshold(t) => {
                w.u8(2).u16(*t);
            }
            Self::SetTimeLock(t) => {
                w.u8(3).u32(*t);
            }
            Self::AddSpendingLimit {
                create_key,
                vault_index,
                mint,
                amount,
                period,
                members,
                destinations,
            } => {
                w.u8(4)
                    .pubkey(create_key)
                    .u8(*vault_index)
                    .pubkey(mint)
                    .u64(*amount)
                    .u8(*period as u8)
                    .pubkeys(members)
                    .pubkeys(destinations);
            }
            Self::RemoveSpendingLimit(k) => {
                w.u8(5).pubkey(k);
            }
            Self::SetRentCollector(k) => {
                w.u8(6).option_pubkey(k.as_ref());
            }
        }
    }
}

/// `config_transaction_create(actions, memo)`: config transaction `index` of an autonomous
/// multisig (it changes the multisig itself once approved, time-locked and executed).
pub fn config_transaction_create(
    multisig: &Pubkey,
    index: u64,
    creator: &Pubkey,
    rent_payer: &Pubkey,
    actions: &[ConfigAction],
    memo: Option<&str>,
) -> Instruction {
    let mut w = Writer::with(&disc::CONFIG_TRANSACTION_CREATE);
    w.len(actions.len());
    for a in actions {
        a.write(&mut w);
    }
    w.option_string(memo);
    Instruction {
        program_id: ID,
        accounts: vec![
            AccountMeta::new(*multisig, false),
            AccountMeta::new(pda::transaction(multisig, index), false),
            AccountMeta::new_readonly(*creator, true),
            AccountMeta::new(*rent_payer, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
        ],
        data: w.into_inner(),
    }
}

/// `config_transaction_execute()` of config transaction `index` by `member` (Execute).
/// `rent_payer` (with the system program) pays a realloc of the multisig (`AddMember`); `None`
/// passes neither (Anchor's "none": the program id).
pub fn config_transaction_execute(
    multisig: &Pubkey,
    index: u64,
    member: &Pubkey,
    rent_payer: Option<&Pubkey>,
) -> Instruction {
    let (payer, system) = match rent_payer {
        Some(p) => (
            AccountMeta::new(*p, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
        ),
        None => (
            AccountMeta::new_readonly(ID, false),
            AccountMeta::new_readonly(ID, false),
        ),
    };
    Instruction {
        program_id: ID,
        accounts: vec![
            AccountMeta::new(*multisig, false),
            AccountMeta::new_readonly(*member, true),
            AccountMeta::new(pda::proposal(multisig, index), false),
            AccountMeta::new_readonly(pda::transaction(multisig, index), false),
            payer,
            system,
        ],
        data: disc::CONFIG_TRANSACTION_EXECUTE.to_vec(),
    }
}

/// `true` if `ix` is a Squads `vault_transaction_execute` (I4 reads the vault path through it).
pub fn is_vault_transaction_execute(ix: &Instruction) -> bool {
    ix.program_id == ID && ix.data.get(..8) == Some(&disc::VAULT_TRANSACTION_EXECUTE[..])
}
