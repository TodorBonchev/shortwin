//! Squads v4 governance on the fork (M10 WP-7; ARCH §1.1 "Admin", §3.1, §8.5): the mainnet
//! Squads v4 ELF ([`Env::with_squads`], group `squads_programs`) driven by the
//! `squads-v4-ix` encoders, every transaction through [`Env::send`] so the invariant hooks run
//! on what a vault executes (I4's vault path: [`crate::invariants::is_admin_signed`]).
//!
//! - [`SquadsMs`]: one autonomous multisig and its members' keys. `create`; `stage` (one
//!   transaction: `vault_transaction_create` + `proposal_create` + the creator's
//!   `proposal_approve`), `stage_buffered` (the transaction-buffer path for a message too large
//!   for that transaction), `approve`, `execute`, `approve_and_execute` (one transaction; time
//!   lock 0), `cancel` (`proposal_cancel_v2`), `run` (the whole flow, warping the time lock),
//!   config transactions (`config`, `config_execute`), and decoded reads.
//! - **Fee payer = the acting member** (as in production, where no separate fee payer co-signs;
//!   a separate one would add 96 bytes to every transaction). [`SquadsMs`] swaps
//!   [`Env::payer`] for the member around each [`Env::send`], measures the legacy wire size
//!   first and **refuses to send anything above 1,232 bytes** (the packet limit LiteSVM does not
//!   enforce), and logs every size ([`SquadsMs::sizes`]).
//! - [`Governance`]: the owner's layout (owner decisions 2026-10-02, Q4): **Governance**
//!   autonomous 3-of-5, time lock 172,800 s, vault 0 = `governance` and the upgrade authority of
//!   short.win (the only program since `complete_curve`'s retirement, pump-final D2), vault 1 =
//!   `fee_recipient` (pre-funded with its rent-exempt minimum, 890,880 lamports);
//!   **Emergency** autonomous 2-of-4, time lock 0, vault 0 = `pause_authority`;
//!   `coverage_authority` a separate hot key ([`Env::coverage_authority`]); each multisig's
//!   rent collector is its own vault 0; no spending limits. All four authorities are pairwise
//!   distinct. [`Env::squads_governed`] builds it and initializes the config with the vaults.
//! - [`loader`]: the upgradeable loader's `SetAuthority` / `SetAuthorityChecked` / `Upgrade` /
//!   `ExtendProgram` and a planted buffer, for the upgrade-authority transfer and the upgrade
//!   through the Governance vault.

use std::cell::RefCell;

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::bpf_loader_upgradeable;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use shortwin::state::InitConfigArgs;
use solana_account::Account;
use solana_keypair::Keypair;
use solana_signer::Signer;
use squads_v4_ix::state::{Multisig, Proposal, TransactionBuffer};
use squads_v4_ix::{ConfigAction, LookupTable, Member, MultisigCreateArgs, VaultMessage};

use crate::{Env, TxResult, errors, pda};

pub use squads_v4_ix::error::MultisigError;
pub use squads_v4_ix::{self as sq, pda as squads_pda};

/// The legacy packet limit every transaction must fit (Solana's `PACKET_DATA_SIZE`).
pub const TX_MAX: usize = 1_232;

/// Asserts the transaction failed with Squads' `expected` (the failing top-level instruction's
/// custom error).
#[track_caller]
pub fn assert_squads_err(res: &TxResult, expected: MultisigError) {
    assert_eq!(
        errors::custom_code(res),
        Some(expected.code()),
        "expected Squads {expected:?} ({}), transaction {}",
        expected.code(),
        match res {
            Ok(m) => format!("succeeded; logs:\n{}", m.pretty_logs()),
            Err(f) => format!("failed with {:?}; logs:\n{}", f.err, f.meta.pretty_logs()),
        }
    );
}

/// A staged vault transaction: its index and the compiled message (what `execute` passes).
#[derive(Clone, Debug)]
pub struct Staged {
    pub index: u64,
    pub vault_index: u8,
    pub message: VaultMessage,
}

/// One autonomous Squads multisig on the VM and its members' keys (module docs).
pub struct SquadsMs {
    /// The multisig PDA.
    pub key: Pubkey,
    /// Its members' keys, in creation order (a test that rotates members edits this list).
    pub members: Vec<Keypair>,
    sizes: RefCell<Vec<usize>>,
}

impl SquadsMs {
    /// `multisig_create_v2`: an **autonomous** multisig (no config authority) of `n` new funded
    /// members with every permission, `threshold`, `time_lock`, rent collector = its own vault
    /// 0. Member 0 creates it (and pays).
    pub fn create(env: &mut Env, n: usize, threshold: u16, time_lock: u32) -> Self {
        let members: Vec<Keypair> = (0..n).map(|_| env.funded_keypair()).collect();
        let create_key = Keypair::new();
        let key = squads_pda::multisig(&create_key.pubkey());
        let args = MultisigCreateArgs {
            config_authority: None,
            threshold,
            members: members.iter().map(|m| Member::full(m.pubkey())).collect(),
            time_lock,
            rent_collector: Some(squads_pda::vault(&key, 0)),
            memo: None,
        };
        let ms = Self {
            key,
            members,
            sizes: RefCell::new(Vec::new()),
        };
        let creator = ms.members[0].insecure_clone();
        let ix = sq::ix::multisig_create_v2(
            &sq::MAINNET_TREASURY,
            &create_key.pubkey(),
            &creator.pubkey(),
            &args,
        );
        errors::ok(ms.send(env, &creator, &[ix], &[&create_key]));
        ms
    }

    /// Vault `index`.
    pub fn vault(&self, index: u8) -> Pubkey {
        squads_pda::vault(&self.key, index)
    }

    /// Member `i`'s key (cloned).
    pub fn member(&self, i: usize) -> Keypair {
        self.members[i].insecure_clone()
    }

    /// The decoded `Multisig` account.
    pub fn state(&self, env: &Env) -> Multisig {
        let acc = env.svm.get_account(&self.key).expect("no multisig account");
        Multisig::decode(&acc.data).expect("decode Multisig")
    }

    /// The decoded proposal of transaction `index`, if it exists.
    pub fn proposal(&self, env: &Env, index: u64) -> Option<Proposal> {
        let acc = env
            .svm
            .get_account(&squads_pda::proposal(&self.key, index))?;
        Proposal::decode(&acc.data)
    }

    /// The decoded transaction buffer `buffer_index` of member `creator`, if it exists.
    pub fn buffer(&self, env: &Env, creator: usize, buffer_index: u8) -> Option<TransactionBuffer> {
        let key = squads_pda::transaction_buffer(
            &self.key,
            &self.members[creator].pubkey(),
            buffer_index,
        );
        TransactionBuffer::decode(&env.svm.get_account(&key)?.data)
    }

    /// The index the next transaction gets.
    pub fn next_index(&self, env: &Env) -> u64 {
        self.state(env).transaction_index + 1
    }

    /// The wire size of every transaction sent so far, in order.
    pub fn sizes(&self) -> Vec<usize> {
        self.sizes.borrow().clone()
    }

    /// The wire size of the last transaction sent.
    pub fn last_size(&self) -> usize {
        *self.sizes.borrow().last().expect("nothing sent yet")
    }

    /// How many transactions were sent so far.
    pub fn sent(&self) -> usize {
        self.sizes.borrow().len()
    }

    /// Sends `ixs` paid by `payer` (a member) and signed by `signers` too, through
    /// [`Env::send`]; panics if the transaction exceeds [`TX_MAX`].
    pub fn send(
        &self,
        env: &mut Env,
        payer: &Keypair,
        ixs: &[Instruction],
        signers: &[&Keypair],
    ) -> TxResult {
        let size = wire_size(&payer.pubkey(), ixs);
        assert!(
            size <= TX_MAX,
            "a {size}-byte transaction (limit {TX_MAX}): use the buffer path"
        );
        self.sizes.borrow_mut().push(size);
        let saved = std::mem::replace(&mut env.payer, payer.insecure_clone());
        let res = env.send(ixs, signers);
        env.payer = saved;
        res
    }

    /// `ixs` compiled for vault `vault_index`, loading what `tables` hold.
    pub fn compile(
        &self,
        vault_index: u8,
        ixs: &[Instruction],
        tables: &[LookupTable],
    ) -> VaultMessage {
        sq::compile(&self.vault(vault_index), ixs, tables).expect("compile the vault message")
    }

    /// The instructions of [`SquadsMs::stage`]'s transaction for `message` as transaction
    /// `index`, created (and approved) by member `creator`.
    pub fn stage_ixs(
        &self,
        index: u64,
        message: &VaultMessage,
        vault_index: u8,
        creator: usize,
    ) -> Vec<Instruction> {
        let c = self.members[creator].pubkey();
        vec![
            sq::ix::vault_transaction_create(
                &self.key,
                index,
                &c,
                &c,
                vault_index,
                0,
                &message.bytes,
                None,
            ),
            sq::ix::proposal_create(&self.key, index, &c, &c, false),
            sq::ix::proposal_approve(&self.key, index, &c),
        ]
    }

    /// Stages `ixs` for vault 0 in one transaction by member 0: `vault_transaction_create`,
    /// `proposal_create` (Active) and member 0's approval.
    pub fn stage(&self, env: &mut Env, ixs: &[Instruction]) -> Staged {
        self.stage_with(env, 0, ixs, &[])
    }

    /// [`SquadsMs::stage`] for vault `vault_index`, the message loading what `tables` hold.
    pub fn stage_with(
        &self,
        env: &mut Env,
        vault_index: u8,
        ixs: &[Instruction],
        tables: &[LookupTable],
    ) -> Staged {
        let index = self.next_index(env);
        let message = self.compile(vault_index, ixs, tables);
        let tx = self.stage_ixs(index, &message, vault_index, 0);
        let m0 = self.member(0);
        errors::ok(self.send(env, &m0, &tx, &[]));
        Staged {
            index,
            vault_index,
            message,
        }
    }

    /// The transaction-buffer path for a message too large to stage in one transaction:
    /// member 0 writes it into its buffer `buffer_index` (`transaction_buffer_create` with as
    /// much as fits, then `transaction_buffer_extend`s), then — in one transaction —
    /// `vault_transaction_create_from_buffer` (hash and size checked; the buffer closes),
    /// `proposal_create` and member 0's approval. `then` (members' indexes) adds their
    /// approvals to that last transaction, and `execute_by` the execution (time lock 0 only):
    /// with both, a 2-of-N Emergency acts in two transactions.
    pub fn stage_buffered(
        &self,
        env: &mut Env,
        vault_index: u8,
        ixs: &[Instruction],
        buffer_index: u8,
        then: &[usize],
        execute_by: Option<usize>,
    ) -> (Staged, TxResult) {
        let index = self.next_index(env);
        let message = self.compile(vault_index, ixs, &[]);
        let m0 = self.member(0);
        let c = m0.pubkey();
        let bytes = &message.bytes;
        let hash = crate::fork::sha256(bytes);
        let final_size = u16::try_from(bytes.len()).expect("message over u16");
        // As much as fits in the create, then extends.
        let create = |chunk: &[u8]| {
            sq::ix::transaction_buffer_create(
                &self.key,
                &c,
                &c,
                buffer_index,
                vault_index,
                hash,
                final_size,
                chunk,
            )
        };
        let fit = |build: &dyn Fn(&[u8]) -> Instruction, rest: &[u8]| -> usize {
            let size = |n: usize| wire_size(&c, &[build(&rest[..n])]);
            let mut n = rest.len();
            // One byte of chunk is one byte of transaction (bar a length prefix's growth).
            n = n.saturating_sub(size(n).saturating_sub(TX_MAX));
            while n > 0 && size(n) > TX_MAX {
                n -= 1;
            }
            n
        };
        let mut at = fit(&create, bytes);
        errors::ok(self.send(env, &m0, &[create(&bytes[..at])], &[]));
        while at < bytes.len() {
            let extend = |chunk: &[u8]| {
                sq::ix::transaction_buffer_extend(&self.key, &c, buffer_index, chunk)
            };
            let n = fit(&extend, &bytes[at..]);
            errors::ok(self.send(env, &m0, &[extend(&bytes[at..at + n])], &[]));
            at += n;
        }
        let mut tx = vec![
            sq::ix::vault_transaction_create_from_buffer(
                &self.key,
                index,
                &c,
                &c,
                buffer_index,
                vault_index,
                0,
                None,
            ),
            sq::ix::proposal_create(&self.key, index, &c, &c, false),
            sq::ix::proposal_approve(&self.key, index, &c),
        ];
        let mut signers: Vec<Keypair> = Vec::new();
        for &i in then {
            tx.push(sq::ix::proposal_approve(
                &self.key,
                index,
                &self.members[i].pubkey(),
            ));
            signers.push(self.member(i));
        }
        let staged = Staged {
            index,
            vault_index,
            message,
        };
        if let Some(e) = execute_by {
            tx.push(sq::ix::vault_transaction_execute(
                &self.key,
                index,
                &self.members[e].pubkey(),
                &staged.message,
            ));
            signers.push(self.member(e));
        }
        let refs: Vec<&Keypair> = signers.iter().collect();
        let res = self.send(env, &m0, &tx, &refs);
        (staged, res)
    }

    /// `proposal_approve` of transaction `index` by member `member`.
    pub fn approve(&self, env: &mut Env, index: u64, member: usize) -> TxResult {
        let m = self.member(member);
        let ix = sq::ix::proposal_approve(&self.key, index, &m.pubkey());
        self.send(env, &m, &[ix], &[])
    }

    /// `vault_transaction_execute` of `staged` by member `member`; `extra` signs too (a
    /// non-vault signer of the message, e.g. the deployer handing over the upgrade authority).
    pub fn execute(
        &self,
        env: &mut Env,
        staged: &Staged,
        member: usize,
        extra: &[&Keypair],
    ) -> TxResult {
        let m = self.member(member);
        let ix = self.execute_ix(staged, member);
        self.send(env, &m, &[ix], extra)
    }

    /// The `vault_transaction_execute` instruction of `staged` by member `member`.
    pub fn execute_ix(&self, staged: &Staged, member: usize) -> Instruction {
        sq::ix::vault_transaction_execute(
            &self.key,
            staged.index,
            &self.members[member].pubkey(),
            &staged.message,
        )
    }

    /// Member `member`'s approval and its execution of `staged` in one transaction (the
    /// approval that reaches the threshold; time lock 0).
    pub fn approve_and_execute(
        &self,
        env: &mut Env,
        staged: &Staged,
        member: usize,
        extra: &[&Keypair],
    ) -> TxResult {
        let m = self.member(member);
        let tx = [
            sq::ix::proposal_approve(&self.key, staged.index, &m.pubkey()),
            self.execute_ix(staged, member),
        ];
        self.send(env, &m, &tx, extra)
    }

    /// `proposal_cancel_v2` of transaction `index` (an `Approved` proposal) by member `member`.
    pub fn cancel(&self, env: &mut Env, index: u64, member: usize) -> TxResult {
        let m = self.member(member);
        let ix = sq::ix::proposal_cancel_v2(&self.key, index, &m.pubkey());
        self.send(env, &m, &[ix], &[])
    }

    /// Approvals by members `1..threshold − 1` after [`SquadsMs::stage`]'s (member 0), leaving
    /// the proposal one approval short; returns the next member's index.
    pub fn approve_to_one_short(&self, env: &mut Env, index: u64) -> usize {
        let t = usize::from(self.state(env).threshold);
        for m in 1..t.saturating_sub(1) {
            errors::ok(self.approve(env, index, m));
        }
        t.saturating_sub(1).max(1)
    }

    /// Approvals by members `1..threshold` after [`SquadsMs::stage`]'s: the proposal is
    /// `Approved` now (the time lock starts).
    pub fn approve_to_threshold(&self, env: &mut Env, index: u64) {
        let last = self.approve_to_one_short(env, index);
        errors::ok(self.approve(env, index, last));
    }

    /// The whole flow for `ixs` on vault 0: [`SquadsMs::stage`], the approvals up to the
    /// threshold, the time lock warped (if any), then the execution by the last approver (with
    /// a time lock 0 the threshold approval and the execution share one transaction). Returns
    /// the execution's result.
    pub fn run(&self, env: &mut Env, ixs: &[Instruction], extra: &[&Keypair]) -> TxResult {
        let staged = self.stage(env, ixs);
        self.finish(env, &staged, extra)
    }

    /// [`SquadsMs::run`] after the staging.
    pub fn finish(&self, env: &mut Env, staged: &Staged, extra: &[&Keypair]) -> TxResult {
        let last = self.approve_to_one_short(env, staged.index);
        let time_lock = self.state(env).time_lock;
        if time_lock == 0 {
            return self.approve_and_execute(env, staged, last, extra);
        }
        errors::ok(self.approve(env, staged.index, last));
        env.warp(i64::from(time_lock));
        self.execute(env, staged, last, extra)
    }

    /// Stages config transaction `actions` (one transaction by member 0: create, proposal,
    /// approval); returns its index.
    pub fn config(&self, env: &mut Env, actions: &[ConfigAction]) -> u64 {
        let index = self.next_index(env);
        let m0 = self.member(0);
        let c = m0.pubkey();
        let tx = [
            sq::ix::config_transaction_create(&self.key, index, &c, &c, actions, None),
            sq::ix::proposal_create(&self.key, index, &c, &c, false),
            sq::ix::proposal_approve(&self.key, index, &c),
        ];
        errors::ok(self.send(env, &m0, &tx, &[]));
        index
    }

    /// `config_transaction_execute` of `index` by member `member` (who pays any realloc).
    pub fn config_execute(&self, env: &mut Env, index: u64, member: usize) -> TxResult {
        let m = self.member(member);
        let ix =
            sq::ix::config_transaction_execute(&self.key, index, &m.pubkey(), Some(&m.pubkey()));
        self.send(env, &m, &[ix], &[])
    }
}

/// The legacy wire size of `ixs` paid by `payer` (signatures included).
pub fn wire_size(payer: &Pubkey, ixs: &[Instruction]) -> usize {
    crate::raydium::legacy_size(payer, ixs).0
}

// ---- the owner's layout -------------------------------------------------------------------

/// The owner's Squads layout (module docs; owner decisions 2026-10-02, Q4).
pub struct Governance {
    /// Governance: 3-of-5, time lock 172,800 s.
    pub gov: SquadsMs,
    /// Emergency: 2-of-4, time lock 0.
    pub emg: SquadsMs,
}

impl Governance {
    pub const GOV_MEMBERS: usize = 5;
    pub const GOV_THRESHOLD: u16 = 3;
    pub const GOV_TIME_LOCK: u32 = 172_800;
    pub const EMG_MEMBERS: usize = 4;
    pub const EMG_THRESHOLD: u16 = 2;
    pub const EMG_TIME_LOCK: u32 = 0;
    /// The fee-recipient vault's pre-funding: the rent-exempt minimum of a 0-byte account.
    pub const FEE_VAULT_PREFUND: u64 = 890_880;

    /// Creates both multisigs and pre-funds the fee-recipient vault.
    pub fn create(env: &mut Env) -> Self {
        let gov = SquadsMs::create(
            env,
            Self::GOV_MEMBERS,
            Self::GOV_THRESHOLD,
            Self::GOV_TIME_LOCK,
        );
        let emg = SquadsMs::create(
            env,
            Self::EMG_MEMBERS,
            Self::EMG_THRESHOLD,
            Self::EMG_TIME_LOCK,
        );
        assert_eq!(
            env.svm.minimum_balance_for_rent_exemption(0),
            Self::FEE_VAULT_PREFUND
        );
        let g = Self { gov, emg };
        env.fund(&g.fee_recipient(), Self::FEE_VAULT_PREFUND);
        g
    }

    /// `governance`: Governance vault 0 (also short.win's upgrade authority once moved).
    pub fn governance(&self) -> Pubkey {
        self.gov.vault(0)
    }

    /// `fee_recipient`: Governance vault 1.
    pub fn fee_recipient(&self) -> Pubkey {
        self.gov.vault(1)
    }

    /// `pause_authority`: Emergency vault 0.
    pub fn pause_authority(&self) -> Pubkey {
        self.emg.vault(0)
    }

    /// [`Env::default_init_args`] with the vaults as authorities and the harness's
    /// `coverage_authority` hot key installed (all four pairwise distinct).
    pub fn init_args(&self, env: &Env) -> InitConfigArgs {
        let mut args = env.default_init_args();
        args.governance = self.governance();
        args.pause_authority = self.pause_authority();
        args.fee_recipient = self.fee_recipient();
        args.coverage_authority = env.coverage_authority.pubkey();
        args
    }

    /// The four authorities `(governance, pause_authority, coverage_authority, fee_recipient)`
    /// as the config holds them.
    pub fn authorities(env: &Env) -> [Pubkey; 4] {
        let c = env.config();
        [
            c.governance,
            c.pause_authority,
            c.coverage_authority,
            c.fee_recipient,
        ]
    }

    /// Moves short.win's upgrade authority from the deployer ([`Env::upgrade_authority`]) to
    /// the Governance vault with `SetAuthorityChecked` (the new authority must sign, so a
    /// mistyped vault cannot receive it) inside a Governance vault transaction; the deployer
    /// co-signs the execution after the time lock. Returns the execution's result.
    pub fn transfer_upgrade_authority(&self, env: &mut Env) -> TxResult {
        let deployer = env.upgrade_authority.insecure_clone();
        let ix = loader::set_authority_checked(
            &pda::program_data(),
            &deployer.pubkey(),
            &self.governance(),
        );
        self.gov.run(env, &[ix], &[&deployer])
    }
}

impl Env {
    /// [`Env::with_squads`], the owner's [`Governance`] layout, and `initialize_config` by the
    /// deployer with the vaults ([`Governance::init_args`]). short.win's upgrade authority is
    /// still the deployer ([`Governance::transfer_upgrade_authority`] moves it).
    pub fn squads_governed() -> (Self, Governance) {
        let mut env = Self::with_squads();
        let g = Governance::create(&mut env);
        let args = g.init_args(&env);
        errors::ok(env.initialize_config(args));
        (env, g)
    }
}

// ---- the upgradeable loader -----------------------------------------------------------------

/// The upgradeable loader's instructions the governance uses (`UpgradeableLoaderInstruction`,
/// bincode: a `u32` tag, then the fields) and a planted buffer.
pub mod loader {
    use super::*;

    /// The loader program.
    pub const ID: Pubkey = bpf_loader_upgradeable::ID;
    const RENT: Pubkey =
        anchor_lang::prelude::pubkey!("SysvarRent111111111111111111111111111111111");
    const CLOCK: Pubkey =
        anchor_lang::prelude::pubkey!("SysvarC1ock11111111111111111111111111111111");
    /// `UpgradeableLoaderState::Buffer` metadata: tag 1, `Some(authority)`.
    pub const BUFFER_METADATA: usize = 37;

    /// `SetAuthorityChecked` (tag 7): `current` hands `account`'s authority to `new`; both
    /// sign.
    pub fn set_authority_checked(account: &Pubkey, current: &Pubkey, new: &Pubkey) -> Instruction {
        Instruction {
            program_id: ID,
            accounts: vec![
                AccountMeta::new(*account, false),
                AccountMeta::new_readonly(*current, true),
                AccountMeta::new_readonly(*new, true),
            ],
            data: 7u32.to_le_bytes().to_vec(),
        }
    }

    /// `SetAuthority` (tag 4): `current` hands `account`'s authority to `new` (`None`:
    /// immutable); only `current` signs.
    pub fn set_authority(account: &Pubkey, current: &Pubkey, new: Option<&Pubkey>) -> Instruction {
        let mut accounts = vec![
            AccountMeta::new(*account, false),
            AccountMeta::new_readonly(*current, true),
        ];
        if let Some(n) = new {
            accounts.push(AccountMeta::new_readonly(*n, false));
        }
        Instruction {
            program_id: ID,
            accounts,
            data: 4u32.to_le_bytes().to_vec(),
        }
    }

    /// `Upgrade` (tag 3) of short.win from `buffer`, the surplus lamports to `spill`, signed
    /// by `authority`.
    pub fn upgrade(buffer: &Pubkey, spill: &Pubkey, authority: &Pubkey) -> Instruction {
        Instruction {
            program_id: ID,
            accounts: vec![
                AccountMeta::new(pda::program_data(), false),
                AccountMeta::new(shortwin::ID, false),
                AccountMeta::new(*buffer, false),
                AccountMeta::new(*spill, false),
                AccountMeta::new_readonly(RENT, false),
                AccountMeta::new_readonly(CLOCK, false),
                AccountMeta::new_readonly(*authority, true),
            ],
            data: 3u32.to_le_bytes().to_vec(),
        }
    }

    /// `ExtendProgram { additional_bytes }` (tag 6) of short.win's ProgramData, `payer` paying
    /// the rent (permissionless in Agave 4.2; at least 10 KiB under SIMD-0431).
    pub fn extend_program(payer: &Pubkey, additional_bytes: u32) -> Instruction {
        let mut data = 6u32.to_le_bytes().to_vec();
        data.extend_from_slice(&additional_bytes.to_le_bytes());
        Instruction {
            program_id: ID,
            accounts: vec![
                AccountMeta::new(pda::program_data(), false),
                AccountMeta::new(shortwin::ID, false),
                AccountMeta::new_readonly(anchor_lang::system_program::ID, false),
                AccountMeta::new(*payer, true),
            ],
            data,
        }
    }

    /// Writes a loader buffer holding `elf` with `authority` at a new key (what `solana program
    /// write-buffer` followed by `set-buffer-authority` leaves). Returns its key.
    pub fn plant_buffer(env: &mut Env, elf: &[u8], authority: &Pubkey) -> Pubkey {
        let mut data = vec![1, 0, 0, 0, 1];
        data.extend_from_slice(authority.as_ref());
        data.extend_from_slice(elf);
        let key = Pubkey::new_unique();
        let lamports = env.svm.minimum_balance_for_rent_exemption(data.len());
        env.svm
            .set_account(
                key,
                Account {
                    lamports,
                    data,
                    owner: ID,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .unwrap_or_else(|e| panic!("plant buffer: {e:?}"));
        key
    }

    /// short.win's ProgramData header: `(deploy slot, upgrade authority)`.
    pub fn program_data(env: &Env) -> (u64, Option<Pubkey>) {
        let acc = env
            .svm
            .get_account(&pda::program_data())
            .expect("no ProgramData");
        let h = shortwin_venue_state::program_data::read(&acc.owner.to_bytes(), &acc.data)
            .expect("ProgramData header");
        (h.slot, h.upgrade_authority.map(Pubkey::new_from_array))
    }

    /// The length of short.win's ProgramData account.
    pub fn program_data_len(env: &Env) -> usize {
        env.svm
            .get_account(&pda::program_data())
            .expect("no ProgramData")
            .data
            .len()
    }
}
