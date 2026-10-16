//! The encoders, decoders and error codes against the vendored Squads IDL 2.1.0
//! (`tests/fixtures/idl/squads_multisig_program.json`, sha256-pinned in the fixture manifest).
//!
//! The IDL is Anchor 0.29's legacy format: no discriminators, so each is recomputed as
//! `sha256("global:<snake_name>")[..8]` / `sha256("account:<Name>")[..8]`. A small IDL-driven
//! Borsh encoder ([`encode`]) writes the reference bytes for the same arguments each builder
//! takes, and the builders' accounts are compared with the IDL's (flattened, in order: name
//! count, `isMut`, `isSigner`).

use std::collections::HashMap;
use std::str::FromStr;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use solana_instruction::{AccountMeta, Instruction};
use squads_v4_ix::error::MultisigError;
use squads_v4_ix::ix::{self, ConfigAction, Member, MultisigCreateArgs, Period};
use squads_v4_ix::state::{
    Multisig, Proposal, ProposalStatus, TransactionBuffer, VaultTransaction,
};
use squads_v4_ix::{ID, Pubkey, compile, disc, pda};

fn idl() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/idl/squads_multisig_program.json"
    );
    let text = std::fs::read_to_string(path).expect("read the vendored IDL");
    serde_json::from_str(&text).expect("IDL json")
}

fn sha8(preimage: &str) -> [u8; 8] {
    Sha256::digest(preimage.as_bytes())[..8].try_into().unwrap()
}

fn snake(camel: &str) -> String {
    let mut s = String::new();
    for (i, c) in camel.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                s.push('_');
            }
            s.push(c.to_ascii_lowercase());
        } else {
            s.push(c);
        }
    }
    s
}

struct Idl {
    raw: Value,
    types: HashMap<String, Value>,
}

impl Idl {
    fn load() -> Self {
        let raw = idl();
        let mut types = HashMap::new();
        for t in raw["types"]
            .as_array()
            .unwrap()
            .iter()
            .chain(raw["accounts"].as_array().unwrap())
        {
            types.insert(t["name"].as_str().unwrap().to_owned(), t["type"].clone());
        }
        Self { raw, types }
    }

    fn instruction(&self, snake_name: &str) -> &Value {
        self.raw["instructions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| snake(i["name"].as_str().unwrap()) == snake_name)
            .unwrap_or_else(|| panic!("no instruction {snake_name} in the IDL"))
    }

    /// The instruction's accounts, composite groups flattened: (name, isMut, isSigner, isOptional).
    fn accounts(&self, snake_name: &str) -> Vec<(String, bool, bool, bool)> {
        fn flat(v: &Value, out: &mut Vec<(String, bool, bool, bool)>) {
            for a in v.as_array().unwrap() {
                if let Some(inner) = a.get("accounts") {
                    flat(inner, out);
                } else {
                    out.push((
                        a["name"].as_str().unwrap().to_owned(),
                        a["isMut"].as_bool().unwrap(),
                        a["isSigner"].as_bool().unwrap(),
                        a["isOptional"].as_bool().unwrap_or(false),
                    ));
                }
            }
        }
        let mut out = Vec::new();
        flat(&self.instruction(snake_name)["accounts"], &mut out);
        out
    }

    /// Borsh of `val` as IDL type `ty`.
    fn encode(&self, ty: &Value, val: &Value, out: &mut Vec<u8>) {
        if let Some(s) = ty.as_str() {
            match s {
                "u8" => out.push(val.as_u64().unwrap() as u8),
                "bool" => out.push(u8::from(val.as_bool().unwrap())),
                "u16" => out.extend_from_slice(&(val.as_u64().unwrap() as u16).to_le_bytes()),
                "u32" => out.extend_from_slice(&(val.as_u64().unwrap() as u32).to_le_bytes()),
                "u64" => out.extend_from_slice(&val.as_u64().unwrap().to_le_bytes()),
                "i64" => out.extend_from_slice(&val.as_i64().unwrap().to_le_bytes()),
                "publicKey" => {
                    out.extend_from_slice(Pubkey::from_str(val.as_str().unwrap()).unwrap().as_ref())
                }
                "bytes" => {
                    let b: Vec<u8> = val
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|x| x.as_u64().unwrap() as u8)
                        .collect();
                    out.extend_from_slice(&(b.len() as u32).to_le_bytes());
                    out.extend_from_slice(&b);
                }
                "string" => {
                    let b = val.as_str().unwrap().as_bytes();
                    out.extend_from_slice(&(b.len() as u32).to_le_bytes());
                    out.extend_from_slice(b);
                }
                other => panic!("IDL type {other} not handled"),
            }
            return;
        }
        if let Some(inner) = ty.get("option") {
            if val.is_null() {
                out.push(0);
            } else {
                out.push(1);
                self.encode(inner, val, out);
            }
        } else if let Some(inner) = ty.get("vec") {
            let items = val.as_array().unwrap();
            out.extend_from_slice(&(items.len() as u32).to_le_bytes());
            for it in items {
                self.encode(inner, it, out);
            }
        } else if let Some(arr) = ty.get("array") {
            let items = val.as_array().unwrap();
            assert_eq!(items.len() as u64, arr[1].as_u64().unwrap());
            for it in items {
                self.encode(&arr[0], it, out);
            }
        } else if let Some(name) = ty.get("defined") {
            let def = &self.types[name.as_str().unwrap()];
            self.encode_defined(def, val, out);
        } else {
            panic!("IDL type {ty} not handled");
        }
    }

    fn encode_defined(&self, def: &Value, val: &Value, out: &mut Vec<u8>) {
        match def["kind"].as_str().unwrap() {
            "struct" => {
                for f in def["fields"].as_array().unwrap() {
                    let name = f["name"].as_str().unwrap();
                    let v = val
                        .get(name)
                        .unwrap_or_else(|| panic!("no field {name} in {val}"));
                    self.encode(&f["type"], v, out);
                }
            }
            "enum" => {
                let (variant, fields) = val.as_object().unwrap().iter().next().unwrap();
                let variants = def["variants"].as_array().unwrap();
                let i = variants
                    .iter()
                    .position(|v| v["name"] == *variant)
                    .unwrap_or_else(|| panic!("no variant {variant}"));
                out.push(i as u8);
                if let Some(fs) = variants[i].get("fields") {
                    for f in fs.as_array().unwrap() {
                        let name = f["name"].as_str().unwrap();
                        self.encode(&f["type"], &fields[name], out);
                    }
                }
            }
            k => panic!("kind {k}"),
        }
    }

    /// The reference instruction data: discriminator ‖ Borsh of `args` (in the IDL's arg order).
    fn data(&self, snake_name: &str, args: &Value) -> Vec<u8> {
        let mut out = sha8(&format!("global:{snake_name}")).to_vec();
        for a in self.instruction(snake_name)["args"].as_array().unwrap() {
            let name = a["name"].as_str().unwrap();
            self.encode(&a["type"], &args[name], &mut out);
        }
        out
    }

    /// The reference account data: `sha256("account:<name>")[..8]` ‖ Borsh of `val`.
    fn account(&self, name: &str, val: &Value) -> Vec<u8> {
        let mut out = sha8(&format!("account:{name}")).to_vec();
        self.encode_defined(&self.types[name], val, &mut out);
        out
    }

    /// Asserts `ix` is `snake_name` as the IDL defines it: program, discriminator and data,
    /// account count and each account's flags (an optional account passed as "none", the
    /// program id, is only counted).
    #[track_caller]
    fn check(&self, snake_name: &str, ix: &Instruction, args: &Value) {
        assert_eq!(ix.program_id, ID);
        assert_eq!(ix.data, self.data(snake_name, args), "{snake_name}: data");
        let want = self.accounts(snake_name);
        let named = &ix.accounts[..want.len().min(ix.accounts.len())];
        assert_eq!(named.len(), want.len(), "{snake_name}: account count");
        for (m, (name, w, s, opt)) in named.iter().zip(&want) {
            if *opt && m.pubkey == ID {
                continue;
            }
            assert_eq!(
                (m.is_writable, m.is_signer),
                (*w, *s),
                "{snake_name}: account {name}"
            );
        }
    }
}

fn k(n: u8) -> Pubkey {
    Pubkey::new_from_array([n; 32])
}

fn s(k: &Pubkey) -> Value {
    json!(k.to_string())
}

#[test]
fn discriminators_are_the_anchor_hashes() {
    let idl = Idl::load();
    for (name, d) in [
        ("multisig_create_v2", disc::MULTISIG_CREATE_V2),
        ("vault_transaction_create", disc::VAULT_TRANSACTION_CREATE),
        ("vault_transaction_execute", disc::VAULT_TRANSACTION_EXECUTE),
        (
            "vault_transaction_create_from_buffer",
            disc::VAULT_TRANSACTION_CREATE_FROM_BUFFER,
        ),
        ("transaction_buffer_create", disc::TRANSACTION_BUFFER_CREATE),
        ("transaction_buffer_extend", disc::TRANSACTION_BUFFER_EXTEND),
        ("transaction_buffer_close", disc::TRANSACTION_BUFFER_CLOSE),
        ("proposal_create", disc::PROPOSAL_CREATE),
        ("proposal_approve", disc::PROPOSAL_APPROVE),
        ("proposal_reject", disc::PROPOSAL_REJECT),
        ("proposal_cancel", disc::PROPOSAL_CANCEL),
        ("proposal_cancel_v2", disc::PROPOSAL_CANCEL_V2),
        ("config_transaction_create", disc::CONFIG_TRANSACTION_CREATE),
        (
            "config_transaction_execute",
            disc::CONFIG_TRANSACTION_EXECUTE,
        ),
    ] {
        assert_eq!(d, sha8(&format!("global:{name}")), "{name}");
        // The instruction exists in the IDL under its camelCase name.
        idl.instruction(name);
    }
    for (name, d) in [
        ("Multisig", disc::ACCOUNT_MULTISIG),
        ("Proposal", disc::ACCOUNT_PROPOSAL),
        ("TransactionBuffer", disc::ACCOUNT_TRANSACTION_BUFFER),
        ("VaultTransaction", disc::ACCOUNT_VAULT_TRANSACTION),
    ] {
        assert_eq!(d, sha8(&format!("account:{name}")), "{name}");
        assert!(idl.types.contains_key(name), "{name} in the IDL accounts");
    }
    assert_eq!(
        idl.raw["metadata"]["address"].as_str(),
        Some(ID.to_string().as_str())
    );
    assert_eq!(idl.raw["version"].as_str(), Some("2.1.0"));
}

#[test]
fn error_codes_are_the_idls() {
    let idl = Idl::load();
    let errors = idl.raw["errors"].as_array().unwrap();
    assert_eq!(errors.len(), MultisigError::ALL.len());
    for (e, ours) in errors.iter().zip(MultisigError::ALL) {
        assert_eq!(e["code"].as_u64(), Some(u64::from(ours.code())));
        assert_eq!(e["name"].as_str(), Some(format!("{ours:?}").as_str()));
    }
}

#[test]
fn multisig_create_v2_matches_the_idl() {
    let idl = Idl::load();
    let members = vec![
        Member::full(k(1)),
        Member {
            key: k(2),
            permissions: ix::permission::VOTE,
        },
    ];
    for (config_authority, rent_collector, memo) in [
        (None, None, None),
        (Some(k(7)), Some(k(8)), Some("short.win governance")),
    ] {
        let args = MultisigCreateArgs {
            config_authority,
            threshold: 2,
            members: members.clone(),
            time_lock: 172_800,
            rent_collector,
            memo: memo.map(str::to_owned),
        };
        let built = ix::multisig_create_v2(&k(9), &k(10), &k(11), &args);
        let reference = json!({ "args": {
            "configAuthority": config_authority.map(|x| x.to_string()),
            "threshold": 2,
            "members": [
                { "key": k(1).to_string(), "permissions": { "mask": 7 } },
                { "key": k(2).to_string(), "permissions": { "mask": 2 } },
            ],
            "timeLock": 172_800,
            "rentCollector": rent_collector.map(|x| x.to_string()),
            "memo": memo,
        }});
        idl.check("multisig_create_v2", &built, &reference);
        assert_eq!(built.accounts[0].pubkey, pda::program_config());
        assert_eq!(built.accounts[2].pubkey, pda::multisig(&k(10)));
    }
}

#[test]
fn vault_transaction_builders_match_the_idl() {
    let idl = Idl::load();
    let (ms, creator, payer) = (k(1), k(2), k(3));
    let message: Vec<u8> = (0..=255u8).collect();
    let built = ix::vault_transaction_create(&ms, 5, &creator, &payer, 1, 2, &message, Some("m"));
    let reference = json!({ "args": {
        "vaultIndex": 1, "ephemeralSigners": 2, "transactionMessage": message, "memo": "m",
    }});
    idl.check("vault_transaction_create", &built, &reference);
    assert_eq!(built.accounts[1].pubkey, pda::transaction(&ms, 5));

    let built = ix::vault_transaction_create_from_buffer(&ms, 6, &creator, &payer, 3, 0, 0, None);
    let reference = json!({ "args": {
        "vaultIndex": 0, "ephemeralSigners": 0, "transactionMessage": [0, 0, 0, 0, 0, 0],
        "memo": null,
    }});
    idl.check("vault_transaction_create_from_buffer", &built, &reference);
    assert_eq!(
        built.accounts[5].pubkey,
        pda::transaction_buffer(&ms, &creator, 3)
    );

    let hash = [9u8; 32];
    let chunk = vec![4u8; 300];
    let built = ix::transaction_buffer_create(&ms, &creator, &payer, 3, 0, hash, 900, &chunk);
    let reference = json!({ "args": {
        "bufferIndex": 3, "vaultIndex": 0, "finalBufferHash": hash.to_vec(),
        "finalBufferSize": 900, "buffer": chunk,
    }});
    idl.check("transaction_buffer_create", &built, &reference);
    let built = ix::transaction_buffer_extend(&ms, &creator, 3, &chunk);
    idl.check(
        "transaction_buffer_extend",
        &built,
        &json!({ "args": { "buffer": chunk } }),
    );
    idl.check(
        "transaction_buffer_close",
        &ix::transaction_buffer_close(&ms, &creator, 3),
        &json!({}),
    );

    // Execute: the named accounts, then the message's (vault not a signer).
    let vault = pda::vault(&ms, 0);
    let inner = Instruction {
        program_id: k(40),
        accounts: vec![
            solana_instruction::AccountMeta::new_readonly(vault, true),
            solana_instruction::AccountMeta::new(k(41), false),
        ],
        data: vec![1],
    };
    let m = compile(&vault, &[inner], &[]).unwrap();
    let built = ix::vault_transaction_execute(&ms, 6, &creator, &m);
    idl.check("vault_transaction_execute", &built, &json!({}));
    assert_eq!(built.accounts.len(), 4 + 3);
    assert_eq!(built.accounts[4].pubkey, vault);
    assert!(!built.accounts[4].is_signer && built.accounts[4].is_writable);
    assert!(ix::is_vault_transaction_execute(&built));
    assert!(!ix::is_vault_transaction_execute(&ix::proposal_approve(
        &ms, 6, &creator
    )));
}

#[test]
fn proposal_builders_match_the_idl() {
    let idl = Idl::load();
    let (ms, member) = (k(1), k(2));
    for draft in [false, true] {
        let built = ix::proposal_create(&ms, 7, &member, &k(3), draft);
        idl.check(
            "proposal_create",
            &built,
            &json!({ "args": { "transactionIndex": 7, "draft": draft } }),
        );
        assert_eq!(built.accounts[1].pubkey, pda::proposal(&ms, 7));
    }
    let none = json!({ "args": { "memo": null } });
    idl.check(
        "proposal_approve",
        &ix::proposal_approve(&ms, 7, &member),
        &none,
    );
    idl.check(
        "proposal_reject",
        &ix::proposal_reject(&ms, 7, &member),
        &none,
    );
    idl.check(
        "proposal_cancel",
        &ix::proposal_cancel(&ms, 7, &member),
        &none,
    );
    idl.check(
        "proposal_cancel_v2",
        &ix::proposal_cancel_v2(&ms, 7, &member),
        &none,
    );
}

#[test]
fn config_transaction_builders_match_the_idl() {
    let idl = Idl::load();
    let (ms, member) = (k(1), k(2));
    let actions = vec![
        ConfigAction::AddMember(Member::full(k(20))),
        ConfigAction::RemoveMember(k(21)),
        ConfigAction::ChangeThreshold(3),
        ConfigAction::SetTimeLock(172_800),
        ConfigAction::AddSpendingLimit {
            create_key: k(22),
            vault_index: 1,
            mint: k(23),
            amount: 5,
            period: Period::Week,
            members: vec![k(24)],
            destinations: vec![k(25), k(26)],
        },
        ConfigAction::RemoveSpendingLimit(k(27)),
        ConfigAction::SetRentCollector(Some(k(28))),
        ConfigAction::SetRentCollector(None),
    ];
    let reference = json!({ "args": {
        "actions": [
            { "AddMember": { "newMember": { "key": k(20).to_string(), "permissions": { "mask": 7 } } } },
            { "RemoveMember": { "oldMember": k(21).to_string() } },
            { "ChangeThreshold": { "newThreshold": 3 } },
            { "SetTimeLock": { "newTimeLock": 172_800 } },
            { "AddSpendingLimit": {
                "createKey": k(22).to_string(), "vaultIndex": 1, "mint": k(23).to_string(),
                "amount": 5, "period": { "Week": {} }, "members": [k(24).to_string()],
                "destinations": [k(25).to_string(), k(26).to_string()],
            } },
            { "RemoveSpendingLimit": { "spendingLimit": k(27).to_string() } },
            { "SetRentCollector": { "newRentCollector": k(28).to_string() } },
            { "SetRentCollector": { "newRentCollector": null } },
        ],
        "memo": "rotate",
    }});
    let built = ix::config_transaction_create(&ms, 4, &member, &k(3), &actions, Some("rotate"));
    idl.check("config_transaction_create", &built, &reference);
    for payer in [Some(k(3)), None] {
        let built = ix::config_transaction_execute(&ms, 4, &member, payer.as_ref());
        idl.check("config_transaction_execute", &built, &json!({}));
        assert_eq!(built.accounts[2].pubkey, pda::proposal(&ms, 4));
        assert_eq!(built.accounts[3].pubkey, pda::transaction(&ms, 4));
    }
}

#[test]
fn decoders_read_what_the_idl_writes() {
    let idl = Idl::load();
    let raw = idl.account(
        "Multisig",
        &json!({
            "createKey": s(&k(1)), "configAuthority": s(&Pubkey::default()), "threshold": 3,
            "timeLock": 172_800, "transactionIndex": 9, "staleTransactionIndex": 4,
            "rentCollector": s(&k(2)), "bump": 254,
            "members": [
                { "key": s(&k(3)), "permissions": { "mask": 7 } },
                { "key": s(&k(4)), "permissions": { "mask": 1 } },
            ],
        }),
    );
    let m = Multisig::decode(&raw).unwrap();
    assert_eq!(
        (m.create_key, m.threshold, m.time_lock, m.transaction_index),
        (k(1), 3, 172_800, 9)
    );
    assert_eq!(
        (m.stale_transaction_index, m.rent_collector, m.bump),
        (4, Some(k(2)), 254)
    );
    assert_eq!(m.config_authority, Pubkey::default());
    assert_eq!(
        m.members,
        vec![
            Member::full(k(3)),
            Member {
                key: k(4),
                permissions: 1
            }
        ]
    );
    assert!(m.is_member(&k(4)) && !m.is_member(&k(5)));
    // Realloc padding after the data is ignored; a wrong discriminator or truncation is not.
    let mut padded = raw.clone();
    padded.extend_from_slice(&[0; 64]);
    assert_eq!(Multisig::decode(&padded), Some(m));
    assert_eq!(Multisig::decode(&raw[..raw.len() - 1]), None);
    let mut wrong = raw.clone();
    wrong[0] ^= 1;
    assert_eq!(Multisig::decode(&wrong), None);

    for (status, want) in [
        (
            json!({ "Active": { "timestamp": 5 } }),
            ProposalStatus::Active(5),
        ),
        (
            json!({ "Approved": { "timestamp": -7 } }),
            ProposalStatus::Approved(-7),
        ),
        (json!({ "Executing": {} }), ProposalStatus::Executing),
        (
            json!({ "Executed": { "timestamp": 8 } }),
            ProposalStatus::Executed(8),
        ),
        (
            json!({ "Cancelled": { "timestamp": 9 } }),
            ProposalStatus::Cancelled(9),
        ),
        (
            json!({ "Rejected": { "timestamp": 1 } }),
            ProposalStatus::Rejected(1),
        ),
        (
            json!({ "Draft": { "timestamp": 2 } }),
            ProposalStatus::Draft(2),
        ),
    ] {
        let raw = idl.account(
            "Proposal",
            &json!({
                "multisig": s(&k(1)), "transactionIndex": 12, "status": status, "bump": 250,
                "approved": [s(&k(2)), s(&k(3))], "rejected": [], "cancelled": [s(&k(4))],
            }),
        );
        let p = Proposal::decode(&raw).unwrap();
        assert_eq!(
            (p.multisig, p.transaction_index, p.status, p.bump),
            (k(1), 12, want, 250)
        );
        assert_eq!(
            (p.approved, p.rejected, p.cancelled),
            (vec![k(2), k(3)], vec![], vec![k(4)])
        );
    }

    let raw = idl.account(
        "TransactionBuffer",
        &json!({
            "multisig": s(&k(1)), "creator": s(&k(2)), "bufferIndex": 3, "vaultIndex": 0,
            "finalBufferHash": vec![6u8; 32], "finalBufferSize": 4, "buffer": [1, 2],
        }),
    );
    let b = TransactionBuffer::decode(&raw).unwrap();
    assert_eq!(
        (b.multisig, b.creator, b.buffer_index, b.vault_index),
        (k(1), k(2), 3, 0)
    );
    assert_eq!(
        (b.final_buffer_hash, b.final_buffer_size, b.buffer),
        ([6; 32], 4, vec![1, 2])
    );
}

/// `VaultTransaction`: the decoder reads what the IDL writes for a message [`compile`] built
/// (with an address lookup table), and [`VaultTransaction::executed_instructions`] over that
/// message's `vault_transaction_execute` remaining accounts gives back the instructions — the
/// vault a writable signer (the payer; Squads signs for it), every other flag as compiled, the
/// table-loaded keys included. A mismatched remaining list is refused.
#[test]
fn vault_transaction_decodes_and_resolves() {
    let idl = Idl::load();
    let vault = k(1);
    let ixs = vec![
        Instruction {
            program_id: k(9),
            accounts: vec![
                // The vault is the message's payer: always writable.
                AccountMeta::new(vault, true),
                AccountMeta::new(k(4), false),
                AccountMeta::new_readonly(k(5), false),
            ],
            data: vec![7, 8],
        },
        Instruction {
            program_id: k(8),
            accounts: vec![AccountMeta::new(k(4), false), AccountMeta::new(k(6), false)],
            data: vec![],
        },
    ];
    let table = squads_v4_ix::LookupTable {
        key: k(100),
        addresses: vec![k(5), k(6)],
    };
    let m = compile(&vault, &ixs, std::slice::from_ref(&table)).unwrap();
    let d = squads_v4_ix::message::decode(&m.bytes).unwrap();
    let instructions: Vec<Value> = d
        .instructions
        .iter()
        .map(|(p, a, data)| json!({ "programIdIndex": p, "accountIndexes": a, "data": data }))
        .collect();
    let lookups: Vec<Value> = d
        .lookups
        .iter()
        .map(|(t, w, r)| json!({ "accountKey": s(t), "writableIndexes": w, "readonlyIndexes": r }))
        .collect();
    let raw = idl.account(
        "VaultTransaction",
        &json!({
            "multisig": s(&k(2)), "creator": s(&k(3)), "index": 7, "bump": 253,
            "vaultIndex": 0, "vaultBump": 255, "ephemeralSignerBumps": [],
            "message": {
                "numSigners": d.num_signers, "numWritableSigners": d.num_writable_signers,
                "numWritableNonSigners": d.num_writable_non_signers,
                "accountKeys": d.account_keys.iter().map(s).collect::<Vec<_>>(),
                "instructions": instructions, "addressTableLookups": lookups,
            },
        }),
    );
    let t = VaultTransaction::decode(&raw).unwrap();
    assert_eq!(
        (t.multisig, t.creator, t.index, t.bump, t.vault_index),
        (k(2), k(3), 7, 253, 0)
    );
    assert_eq!(t.message, d);
    let remaining = m.execute_accounts(&[]);
    let run = t.executed_instructions(&remaining).unwrap();
    assert_eq!(run, ixs);
    // A remaining list that is short, or names another static key, is refused.
    assert_eq!(t.executed_instructions(&remaining[1..]), None);
    let mut swapped = remaining.clone();
    swapped[1].pubkey = k(42);
    assert_eq!(t.executed_instructions(&swapped), None);
    let mut wrong = raw.clone();
    wrong[0] ^= 1;
    assert_eq!(VaultTransaction::decode(&wrong), None);
    assert_eq!(VaultTransaction::decode(&raw[..raw.len() - 1]), None);
}

/// [`VaultTransaction::execute_accounts`] rebuilds, from the stored account alone, the remaining
/// accounts the compiled message gives (the ops tool executes what it reads); a non-vault
/// signer of the message is a co-signer of the execution; a message with lookup tables is left
/// to the caller.
#[test]
fn stored_vault_transaction_gives_its_execute_accounts() {
    let idl = Idl::load();
    let multisig = k(2);
    let vault = pda::vault(&multisig, 0);
    let deployer = k(3);
    let ixs = vec![Instruction {
        program_id: k(9),
        accounts: vec![
            AccountMeta::new(k(4), false),
            AccountMeta::new_readonly(deployer, true),
            // The vault is the message's payer: always writable.
            AccountMeta::new(vault, true),
            AccountMeta::new_readonly(k(5), false),
        ],
        data: vec![7],
    }];
    let stored = |m: &squads_v4_ix::VaultMessage| {
        let d = squads_v4_ix::message::decode(&m.bytes).unwrap();
        let instructions: Vec<Value> = d
            .instructions
            .iter()
            .map(|(p, a, data)| json!({ "programIdIndex": p, "accountIndexes": a, "data": data }))
            .collect();
        let lookups: Vec<Value> = d
            .lookups
            .iter()
            .map(|(t, w, r)| json!({ "accountKey": s(t), "writableIndexes": w, "readonlyIndexes": r }))
            .collect();
        let raw = idl.account(
            "VaultTransaction",
            &json!({
                "multisig": s(&multisig), "creator": s(&k(6)), "index": 3, "bump": 254,
                "vaultIndex": 0, "vaultBump": 255, "ephemeralSignerBumps": [],
                "message": {
                    "numSigners": d.num_signers, "numWritableSigners": d.num_writable_signers,
                    "numWritableNonSigners": d.num_writable_non_signers,
                    "accountKeys": d.account_keys.iter().map(s).collect::<Vec<_>>(),
                    "instructions": instructions, "addressTableLookups": lookups,
                },
            }),
        );
        VaultTransaction::decode(&raw).unwrap()
    };
    let m = compile(&vault, &ixs, &[]).unwrap();
    let t = stored(&m);
    assert_eq!(t.vault(), vault);
    assert!(t.ephemeral_signers().is_empty());
    let remaining = t.execute_accounts().unwrap();
    assert_eq!(remaining, m.execute_accounts(&[]));
    // The vault is never passed as a signer (Squads signs for it); the deployer is.
    assert!(remaining.iter().any(|a| a.pubkey == vault && !a.is_signer));
    assert!(
        remaining
            .iter()
            .any(|a| a.pubkey == deployer && a.is_signer)
    );
    assert_eq!(t.co_signers(), vec![deployer]);
    assert_eq!(t.executed_instructions(&remaining).unwrap(), ixs);
    let exec = ix::vault_transaction_execute_with(&multisig, 3, &k(7), remaining);
    assert_eq!(exec, ix::vault_transaction_execute(&multisig, 3, &k(7), &m));
    // With an address lookup table the stored message alone does not name every key.
    let table = squads_v4_ix::LookupTable {
        key: k(100),
        addresses: vec![k(5)],
    };
    let with_table = compile(&vault, &ixs, std::slice::from_ref(&table)).unwrap();
    assert_eq!(stored(&with_table).execute_accounts(), None);
}

#[test]
fn pdas_follow_the_seeds() {
    // The mainnet ProgramConfig the fixture group holds.
    assert_eq!(
        pda::program_config().to_string(),
        "BSTq9w3kZwNwpBXJEvTZz2G9ZTNyKBvoSeXMvwb4cNZr"
    );
    let ms = pda::multisig(&k(1));
    let (v0, bump) = pda::vault_with_bump(&ms, 0);
    assert_eq!(
        Pubkey::create_program_address(&[b"multisig", ms.as_ref(), b"vault", &[0], &[bump]], &ID)
            .unwrap(),
        v0
    );
    assert_ne!(pda::vault(&ms, 1), v0);
    assert_ne!(pda::transaction(&ms, 1), pda::proposal(&ms, 1));
    assert_ne!(
        pda::transaction_buffer(&ms, &k(2), 0),
        pda::transaction_buffer(&ms, &k(3), 0)
    );
}
