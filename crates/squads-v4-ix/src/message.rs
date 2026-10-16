//! The Squads `TransactionMessage` (`instructions/vault_transaction_create.rs` at `64af733`):
//! what a vault transaction stores and later executes with the vault's signature.
//!
//! Its wire form is Squads' own compact encoding (`utils/small_vec.rs`), not Borsh's:
//!
//! ```text
//! u8 num_signers, u8 num_writable_signers, u8 num_writable_non_signers,
//! u8 n, n × [u8; 32] account_keys,
//! u8 n, n × { u8 program_id_index, u8 m, m × u8 account_indexes, u16 len, len × u8 data },
//! u8 n, n × { [u8; 32] lookup table, u8 w, w × u8 writable_indexes, u8 r, r × u8 readonly_indexes }
//! ```
//!
//! [`compile`] orders the keys as `@solana/web3.js`'s `compileToV0Message` does with the vault as
//! payer, which is what the Squads SDK sends: the vault first (a writable signer), then writable
//! signers, read-only signers, writable non-signers and read-only non-signers, each group in
//! first-use order. With lookup tables, every non-signer that is not a program id and that a
//! table holds is loaded from the first table holding it instead of being a static key. An
//! account index counts the static keys, then every loaded writable key, then every loaded
//! read-only key (tables in order), as Squads resolves them (`get_account_by_index`).
//!
//! [`VaultMessage::execute_accounts`] lists the remaining accounts `vault_transaction_execute`
//! needs, in Squads' order: the lookup tables, the static keys (signer only for a real signer:
//! the vault and ephemeral signers are PDAs, signed for by Squads), then the loaded keys.

use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;

/// An address lookup table the compiler may load keys from: its address and its addresses in
/// table order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LookupTable {
    pub key: Pubkey,
    pub addresses: Vec<Pubkey>,
}

/// One table a compiled message loads from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledLookup {
    pub table: Pubkey,
    pub writable_indexes: Vec<u8>,
    pub readonly_indexes: Vec<u8>,
    /// The keys `writable_indexes` resolve to.
    pub writable: Vec<Pubkey>,
    /// The keys `readonly_indexes` resolve to.
    pub readonly: Vec<Pubkey>,
}

/// A static key of the message with its signer and writable flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageKey {
    pub key: Pubkey,
    pub signer: bool,
    pub writable: bool,
}

/// A compiled Squads `TransactionMessage`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaultMessage {
    /// The vault (or ephemeral) key the message was compiled for: always the first static key.
    pub vault: Pubkey,
    /// The encoded message (`vault_transaction_create`'s `transaction_message`, or a buffer's
    /// content).
    pub bytes: Vec<u8>,
    /// The static keys in message order.
    pub keys: Vec<MessageKey>,
    /// The tables keys are loaded from, in message order.
    pub lookups: Vec<CompiledLookup>,
}

/// Why a set of instructions cannot be one Squads message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageError {
    /// More than 255 static keys, instructions, tables or account indexes of one instruction.
    TooMany(&'static str),
    /// An instruction's data is longer than `u16::MAX`.
    DataTooLong,
}

impl VaultMessage {
    /// The remaining accounts of `vault_transaction_execute`: the lookup tables (read-only),
    /// the static keys with their flags — a signer key is passed as a signer unless it is the
    /// vault or one of `ephemeral_signers` —, then the loaded writable and read-only keys.
    pub fn execute_accounts(&self, ephemeral_signers: &[Pubkey]) -> Vec<AccountMeta> {
        let mut out: Vec<AccountMeta> = self
            .lookups
            .iter()
            .map(|l| AccountMeta::new_readonly(l.table, false))
            .collect();
        for k in &self.keys {
            let signer = k.signer && k.key != self.vault && !ephemeral_signers.contains(&k.key);
            out.push(if k.writable {
                AccountMeta::new(k.key, signer)
            } else {
                AccountMeta::new_readonly(k.key, signer)
            });
        }
        for l in &self.lookups {
            out.extend(l.writable.iter().map(|k| AccountMeta::new(*k, false)));
        }
        for l in &self.lookups {
            out.extend(
                l.readonly
                    .iter()
                    .map(|k| AccountMeta::new_readonly(*k, false)),
            );
        }
        out
    }

    /// Every key the message names (static, then loaded writable, then loaded read-only).
    pub fn all_keys(&self) -> Vec<Pubkey> {
        let mut v: Vec<Pubkey> = self.keys.iter().map(|k| k.key).collect();
        for l in &self.lookups {
            v.extend_from_slice(&l.writable);
        }
        for l in &self.lookups {
            v.extend_from_slice(&l.readonly);
        }
        v
    }
}

fn count(n: usize, what: &'static str) -> Result<u8, MessageError> {
    u8::try_from(n).map_err(|_| MessageError::TooMany(what))
}

/// Compiles `ixs` into a Squads `TransactionMessage` for `vault` (module docs), loading
/// eligible keys from `tables`.
pub fn compile(
    vault: &Pubkey,
    ixs: &[Instruction],
    tables: &[LookupTable],
) -> Result<VaultMessage, MessageError> {
    // Unique keys in first-use order with merged flags; the vault first (the payer).
    struct Entry {
        key: Pubkey,
        signer: bool,
        writable: bool,
        invoked: bool,
    }
    let mut entries: Vec<Entry> = vec![Entry {
        key: *vault,
        signer: true,
        writable: true,
        invoked: false,
    }];
    let mut add = |key: Pubkey, signer: bool, writable: bool, invoked: bool| {
        if let Some(e) = entries.iter_mut().find(|e| e.key == key) {
            e.signer |= signer;
            e.writable |= writable;
            e.invoked |= invoked;
        } else {
            entries.push(Entry {
                key,
                signer,
                writable,
                invoked,
            });
        }
    };
    for ix in ixs {
        add(ix.program_id, false, false, true);
        for m in &ix.accounts {
            add(m.pubkey, m.is_signer, m.is_writable, false);
        }
    }

    // Loaded keys: non-signers, never a program id, from the first table holding them.
    let mut lookups: Vec<CompiledLookup> = Vec::new();
    let mut loaded: Vec<Pubkey> = Vec::new();
    for t in tables {
        let mut l = CompiledLookup {
            table: t.key,
            writable_indexes: Vec::new(),
            readonly_indexes: Vec::new(),
            writable: Vec::new(),
            readonly: Vec::new(),
        };
        for e in &entries {
            if e.signer || e.invoked || loaded.contains(&e.key) {
                continue;
            }
            let Some(i) = t.addresses.iter().position(|a| *a == e.key) else {
                continue;
            };
            let i = count(i, "lookup table index")?;
            if e.writable {
                l.writable_indexes.push(i);
                l.writable.push(e.key);
            } else {
                l.readonly_indexes.push(i);
                l.readonly.push(e.key);
            }
            loaded.push(e.key);
        }
        if !l.writable.is_empty() || !l.readonly.is_empty() {
            lookups.push(l);
        }
    }

    let rank = |e: &Entry| match (e.signer, e.writable) {
        (true, true) => 0,
        (true, false) => 1,
        (false, true) => 2,
        (false, false) => 3,
    };
    let mut statics: Vec<&Entry> = entries
        .iter()
        .filter(|e| !loaded.contains(&e.key))
        .collect();
    // Stable: first-use order within each group (the vault stays first).
    statics.sort_by_key(|e| rank(e));
    let keys: Vec<MessageKey> = statics
        .iter()
        .map(|e| MessageKey {
            key: e.key,
            signer: e.signer,
            writable: e.writable,
        })
        .collect();

    let order: Vec<Pubkey> = {
        let mut v: Vec<Pubkey> = keys.iter().map(|k| k.key).collect();
        for l in &lookups {
            v.extend_from_slice(&l.writable);
        }
        for l in &lookups {
            v.extend_from_slice(&l.readonly);
        }
        v
    };
    let index = |k: &Pubkey| -> Result<u8, MessageError> {
        count(
            order.iter().position(|o| o == k).unwrap_or(usize::MAX),
            "account index",
        )
    };

    let mut out = vec![
        count(keys.iter().filter(|k| k.signer).count(), "signers")?,
        count(
            keys.iter().filter(|k| k.signer && k.writable).count(),
            "writable signers",
        )?,
        count(
            keys.iter().filter(|k| !k.signer && k.writable).count(),
            "writable non-signers",
        )?,
        count(keys.len(), "account keys")?,
    ];
    for k in &keys {
        out.extend_from_slice(k.key.as_ref());
    }
    out.push(count(ixs.len(), "instructions")?);
    for ix in ixs {
        out.push(index(&ix.program_id)?);
        out.push(count(ix.accounts.len(), "instruction accounts")?);
        for m in &ix.accounts {
            out.push(index(&m.pubkey)?);
        }
        let len = u16::try_from(ix.data.len()).map_err(|_| MessageError::DataTooLong)?;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&ix.data);
    }
    out.push(count(lookups.len(), "lookup tables")?);
    for l in &lookups {
        out.extend_from_slice(l.table.as_ref());
        out.push(count(l.writable_indexes.len(), "writable indexes")?);
        out.extend_from_slice(&l.writable_indexes);
        out.push(count(l.readonly_indexes.len(), "readonly indexes")?);
        out.extend_from_slice(&l.readonly_indexes);
    }
    Ok(VaultMessage {
        vault: *vault,
        bytes: out,
        keys,
        lookups,
    })
}

/// A decoded Squads `TransactionMessage` (the inverse of [`compile`]'s encoding).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedMessage {
    pub num_signers: u8,
    pub num_writable_signers: u8,
    pub num_writable_non_signers: u8,
    pub account_keys: Vec<Pubkey>,
    /// `(program_id_index, account_indexes, data)`.
    pub instructions: Vec<(u8, Vec<u8>, Vec<u8>)>,
    /// `(table, writable_indexes, readonly_indexes)`.
    pub lookups: Vec<(Pubkey, Vec<u8>, Vec<u8>)>,
}

/// Decodes a Squads `TransactionMessage`; `None` if it is malformed or has trailing bytes.
pub fn decode(bytes: &[u8]) -> Option<DecodedMessage> {
    let mut r = crate::borsh::Reader::new(bytes);
    let num_signers = r.u8()?;
    let num_writable_signers = r.u8()?;
    let num_writable_non_signers = r.u8()?;
    let n = usize::from(r.u8()?);
    let account_keys = (0..n).map(|_| r.pubkey()).collect::<Option<Vec<_>>>()?;
    let n = usize::from(r.u8()?);
    let mut instructions = Vec::with_capacity(n);
    for _ in 0..n {
        let program = r.u8()?;
        let m = usize::from(r.u8()?);
        let accounts = r.take(m)?.to_vec();
        let len = usize::from(r.u16()?);
        let data = r.take(len)?.to_vec();
        instructions.push((program, accounts, data));
    }
    let n = usize::from(r.u8()?);
    let mut lookups = Vec::with_capacity(n);
    for _ in 0..n {
        let table = r.pubkey()?;
        let w = usize::from(r.u8()?);
        let writable = r.take(w)?.to_vec();
        let ro = usize::from(r.u8()?);
        let readonly = r.take(ro)?.to_vec();
        lookups.push((table, writable, readonly));
    }
    (r.position() == bytes.len()).then_some(DecodedMessage {
        num_signers,
        num_writable_signers,
        num_writable_non_signers,
        account_keys,
        instructions,
        lookups,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(n: u8) -> Pubkey {
        Pubkey::new_from_array([n; 32])
    }

    #[test]
    fn orders_keys_and_round_trips() {
        let vault = k(1);
        let ixs = [
            Instruction {
                program_id: k(9),
                accounts: vec![
                    AccountMeta::new_readonly(k(5), false),
                    AccountMeta::new_readonly(vault, true),
                    AccountMeta::new(k(4), false),
                    AccountMeta::new_readonly(k(3), true),
                ],
                data: vec![1, 2, 3],
            },
            Instruction {
                program_id: k(8),
                accounts: vec![AccountMeta::new(k(5), false)],
                data: vec![],
            },
        ];
        let m = compile(&vault, &ixs, &[]).unwrap();
        let order: Vec<Pubkey> = m.keys.iter().map(|x| x.key).collect();
        // vault (w s), k3 (r s), k5 (w: merged), k4 (w), k9, k8 (programs, read-only).
        assert_eq!(order, vec![vault, k(3), k(5), k(4), k(9), k(8)]);
        let d = decode(&m.bytes).unwrap();
        assert_eq!(
            (
                d.num_signers,
                d.num_writable_signers,
                d.num_writable_non_signers
            ),
            (2, 1, 2)
        );
        assert_eq!(d.account_keys, order);
        assert_eq!(d.instructions[0], (4, vec![2, 0, 3, 1], vec![1, 2, 3]));
        assert_eq!(d.instructions[1], (5, vec![2], vec![]));
        assert!(d.lookups.is_empty());
        // Remaining accounts: the vault is not passed as a signer, k3 is.
        let metas = m.execute_accounts(&[]);
        assert_eq!(metas.len(), 6);
        assert!(!metas[0].is_signer && metas[0].is_writable);
        assert!(metas[1].is_signer && !metas[1].is_writable);
    }

    #[test]
    fn loads_non_signers_from_tables_but_never_program_ids() {
        let vault = k(1);
        let ix = Instruction {
            program_id: k(9),
            accounts: vec![
                AccountMeta::new_readonly(vault, true),
                AccountMeta::new(k(4), false),
                AccountMeta::new_readonly(k(5), false),
                AccountMeta::new_readonly(k(6), false),
            ],
            data: vec![7],
        };
        let t1 = LookupTable {
            key: k(100),
            addresses: vec![k(9), k(6), k(4)],
        };
        let t2 = LookupTable {
            key: k(101),
            addresses: vec![k(5), k(4), vault],
        };
        let m = compile(&vault, std::slice::from_ref(&ix), &[t1, t2]).unwrap();
        let statics: Vec<Pubkey> = m.keys.iter().map(|x| x.key).collect();
        assert_eq!(statics, vec![vault, k(9)]);
        assert_eq!(m.lookups.len(), 2);
        assert_eq!(
            (
                m.lookups[0].writable_indexes.clone(),
                m.lookups[0].readonly_indexes.clone()
            ),
            (vec![2], vec![1])
        );
        assert_eq!(
            (
                m.lookups[1].writable_indexes.clone(),
                m.lookups[1].readonly_indexes.clone()
            ),
            (vec![], vec![0])
        );
        // Index space: statics [vault, k9], loaded writable [k4], loaded read-only [k6, k5].
        assert_eq!(m.all_keys(), vec![vault, k(9), k(4), k(6), k(5)]);
        let d = decode(&m.bytes).unwrap();
        assert_eq!(d.instructions[0], (1, vec![0, 2, 4, 3], vec![7]));
        assert_eq!(d.lookups[0], (k(100), vec![2], vec![1]));
        let metas = m.execute_accounts(&[]);
        let keys: Vec<Pubkey> = metas.iter().map(|a| a.pubkey).collect();
        assert_eq!(keys, vec![k(100), k(101), vault, k(9), k(4), k(6), k(5)]);
        assert!(metas[4].is_writable && !metas[5].is_writable);
    }

    #[test]
    fn refuses_what_does_not_fit() {
        let vault = k(1);
        let big = Instruction {
            program_id: k(9),
            accounts: vec![],
            data: vec![0; usize::from(u16::MAX) + 1],
        };
        assert_eq!(compile(&vault, &[big], &[]), Err(MessageError::DataTooLong));
        assert_eq!(decode(&[0, 0, 0, 1]), None);
        let m = compile(&vault, &[], &[]).unwrap();
        let mut trailing = m.bytes.clone();
        trailing.push(0);
        assert!(decode(&m.bytes).is_some() && decode(&trailing).is_none());
    }
}
