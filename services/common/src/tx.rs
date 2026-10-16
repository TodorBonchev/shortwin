//! Transaction building (ARCHITECTURE §6.4, §7.1 duties 7 and 14).
//!
//! **Formats.** Keeper batches are **v1 transactions** (SIMD-0385: 4,096 B, up to 64 inline
//! addresses, no address lookup tables, the compute budget in the message's config instead of
//! ComputeBudget instructions); where a cluster does not accept v1, the keeper falls back to
//! **v0** (1,232 B, ComputeBudget instructions, optionally the protocol ALT of A28).
//!
//! **Compute budget** (C0-17, duty 14): the unit limit is `⌈simulated × 1.1⌉` ([`cu_limit`]),
//! the price comes from the priority policy (micro-lamports per CU). A v1 message carries the
//! priority fee as a lamport total, `⌈limit·price/10⁶⌉` ([`priority_fee_lamports`]), and needs
//! an explicit loaded-accounts-data limit (its absence means 0 in a v1 config).

use anyhow::{Context, anyhow, bail};
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_hash::Hash;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::{AddressLookupTableAccount, MessageHeader, VersionedMessage, v0, v1};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;

/// Largest legacy / v0 transaction (the packet data size).
pub const V0_MAX_TX_BYTES: usize = 1_232;
/// Largest v1 transaction.
pub const V1_MAX_TX_BYTES: usize = v1::MAX_TRANSACTION_SIZE;
/// Account keys one transaction may lock (the lock limit increase is not active; ARCH §6.4).
pub const MAX_TX_ACCOUNTS: usize = 64;
/// The per-transaction compute ceiling.
pub const MAX_COMPUTE_UNITS: u32 = 1_400_000;
/// The loaded-accounts-data ceiling (64 MiB), the default a v0 transaction gets.
pub const MAX_LOADED_ACCOUNTS_DATA: u32 = 64 * 1024 * 1024;
/// Address of the address-lookup-table program.
pub const ALT_PROGRAM_ID: Pubkey =
    solana_pubkey::pubkey!("AddressLookupTab1e1111111111111111111111111");
/// Size of an address lookup table's metadata header (its addresses follow).
pub const ALT_META_SIZE: usize = 56;

/// A transaction format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TxFormat {
    /// SIMD-0385 v1: inline addresses only, compute budget in the message config.
    V1,
    /// v0: ComputeBudget instructions, optional lookup tables.
    V0,
}

impl TxFormat {
    /// The size limit of a serialized transaction in this format.
    pub const fn max_bytes(self) -> usize {
        match self {
            Self::V1 => V1_MAX_TX_BYTES,
            Self::V0 => V0_MAX_TX_BYTES,
        }
    }
}

/// A transaction's compute budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComputeBudget {
    /// Compute-unit limit.
    pub unit_limit: u32,
    /// Priority price, micro-lamports per compute unit.
    pub unit_price: u64,
}

impl ComputeBudget {
    /// The budget a simulation runs with: the ceiling, no priority price.
    pub const SIMULATION: Self = Self {
        unit_limit: MAX_COMPUTE_UNITS,
        unit_price: 0,
    };
}

/// `⌈simulated·11/10⌉`, at least 1 and at most the ceiling (ARCH §6.1: "keepers use exactly
/// 1.1 × the simulated value").
pub fn cu_limit(simulated: u64) -> u32 {
    let scaled = simulated.saturating_mul(11).div_ceil(10).max(1);
    u32::try_from(scaled)
        .unwrap_or(MAX_COMPUTE_UNITS)
        .min(MAX_COMPUTE_UNITS)
}

/// The priority fee in lamports a budget pays at most: `⌈limit·price/10⁶⌉`.
pub fn priority_fee_lamports(budget: &ComputeBudget) -> u64 {
    let micro = u128::from(budget.unit_limit).saturating_mul(u128::from(budget.unit_price));
    u64::try_from(micro.div_ceil(1_000_000)).unwrap_or(u64::MAX)
}

/// Builds and signs a transaction of `ixs` in `format`. `payer` pays and signs; `signers` are
/// the other keypairs the instructions require (unneeded ones are ignored). `alts` are used
/// by v0 only.
pub fn build(
    format: TxFormat,
    payer: &Keypair,
    signers: &[&Keypair],
    ixs: &[Instruction],
    budget: &ComputeBudget,
    blockhash: Hash,
    alts: &[AddressLookupTableAccount],
) -> anyhow::Result<VersionedTransaction> {
    let payer_key = payer.pubkey();
    let message = match format {
        TxFormat::V1 => {
            let mut config = v1::TransactionConfig::empty()
                .with_compute_unit_limit(budget.unit_limit)
                .with_loaded_accounts_data_size_limit(MAX_LOADED_ACCOUNTS_DATA);
            let fee = priority_fee_lamports(budget);
            if fee > 0 {
                config = config.with_priority_fee(fee);
            }
            VersionedMessage::V1(
                v1::Message::try_compile_with_config(&payer_key, ixs, blockhash, config)
                    .context("compiling a v1 message")?,
            )
        }
        TxFormat::V0 => {
            let mut all = Vec::with_capacity(ixs.len().saturating_add(2));
            all.push(ComputeBudgetInstruction::set_compute_unit_limit(
                budget.unit_limit,
            ));
            if budget.unit_price > 0 {
                all.push(ComputeBudgetInstruction::set_compute_unit_price(
                    budget.unit_price,
                ));
            }
            all.extend_from_slice(ixs);
            VersionedMessage::V0(
                v0::Message::try_compile(&payer_key, &all, alts, blockhash)
                    .context("compiling a v0 message")?,
            )
        }
    };
    let needed = required_signers(&message);
    let mut keys: Vec<&Keypair> = vec![payer];
    for k in needed.iter().skip(1) {
        let s = signers
            .iter()
            .find(|s| s.pubkey() == *k)
            .ok_or_else(|| anyhow!("missing signer {k}"))?;
        keys.push(s);
    }
    VersionedTransaction::try_new(message, &keys).context("signing")
}

fn required_signers(message: &VersionedMessage) -> Vec<Pubkey> {
    let n = usize::from(message.header().num_required_signatures);
    message
        .static_account_keys()
        .iter()
        .take(n)
        .copied()
        .collect()
}

/// Serialized size of `tx` in bytes (the wire encoding).
pub fn size(tx: &VersionedTransaction) -> usize {
    wincode::serialized_size(tx)
        .ok()
        .and_then(|s| usize::try_from(s).ok())
        .unwrap_or(usize::MAX)
}

/// Account keys `tx` locks: the static keys plus, for v0, the lookup-table entries.
pub fn account_count(tx: &VersionedTransaction) -> usize {
    let loaded = tx.message.address_table_lookups().map_or(0, |l| {
        l.iter()
            .map(|t| t.writable_indexes.len() + t.readonly_indexes.len())
            .sum()
    });
    tx.message.static_account_keys().len() + loaded
}

/// `true` if `tx` fits its format's size limit and the account-lock limit.
pub fn fits(format: TxFormat, tx: &VersionedTransaction) -> bool {
    size(tx) <= format.max_bytes() && account_count(tx) <= MAX_TX_ACCOUNTS
}

/// The format of a built transaction.
pub fn format_of(tx: &VersionedTransaction) -> Option<TxFormat> {
    match tx.message {
        VersionedMessage::V1(_) => Some(TxFormat::V1),
        VersionedMessage::V0(_) => Some(TxFormat::V0),
        VersionedMessage::Legacy(_) => None,
    }
}

/// The instructions of `tx` with their account metas (signer and writable flags from the
/// header), for backends and checks that replay a transaction instruction by instruction.
/// `alts` must hold the lookup tables a v0 message names.
pub fn decompile(
    tx: &VersionedTransaction,
    alts: &[AddressLookupTableAccount],
) -> anyhow::Result<Vec<Instruction>> {
    let msg = &tx.message;
    let header: &MessageHeader = msg.header();
    let statics = msg.static_account_keys();
    let n = statics.len();
    let s = usize::from(header.num_required_signatures);
    let rs = usize::from(header.num_readonly_signed_accounts);
    let ru = usize::from(header.num_readonly_unsigned_accounts);
    let mut keys: Vec<(Pubkey, bool, bool)> = statics
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let signer = i < s;
            let writable = if signer {
                i < s.saturating_sub(rs)
            } else {
                i < n.saturating_sub(ru)
            };
            (*k, signer, writable)
        })
        .collect();
    if let Some(lookups) = msg.address_table_lookups() {
        let table = |key: &Pubkey| {
            alts.iter()
                .find(|t| t.key == *key)
                .ok_or_else(|| anyhow!("lookup table {key} not supplied"))
        };
        let mut writable = Vec::new();
        let mut readonly = Vec::new();
        for l in lookups {
            let t = table(&l.account_key)?;
            for i in &l.writable_indexes {
                let k = t.addresses.get(usize::from(*i)).context("lookup index")?;
                writable.push((*k, false, true));
            }
            for i in &l.readonly_indexes {
                let k = t.addresses.get(usize::from(*i)).context("lookup index")?;
                readonly.push((*k, false, false));
            }
        }
        keys.extend(writable);
        keys.extend(readonly);
    }
    msg.instructions()
        .iter()
        .map(|ci| {
            let (program_id, _, _) = *keys
                .get(usize::from(ci.program_id_index))
                .context("program index")?;
            let accounts = ci
                .accounts
                .iter()
                .map(|i| {
                    let (pubkey, is_signer, is_writable) =
                        *keys.get(usize::from(*i)).context("account index")?;
                    Ok(AccountMeta {
                        pubkey,
                        is_signer,
                        is_writable,
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            Ok(Instruction {
                program_id,
                accounts,
                data: ci.data.clone(),
            })
        })
        .collect()
}

/// Every account key of `tx` in message order (static keys, then v0 lookup entries).
pub fn account_keys(
    tx: &VersionedTransaction,
    alts: &[AddressLookupTableAccount],
) -> anyhow::Result<Vec<Pubkey>> {
    let mut keys = tx.message.static_account_keys().to_vec();
    if let Some(lookups) = tx.message.address_table_lookups() {
        let mut writable = Vec::new();
        let mut readonly = Vec::new();
        for l in lookups {
            let t = alts
                .iter()
                .find(|t| t.key == l.account_key)
                .ok_or_else(|| anyhow!("lookup table {} not supplied", l.account_key))?;
            for i in &l.writable_indexes {
                writable.push(*t.addresses.get(usize::from(*i)).context("lookup index")?);
            }
            for i in &l.readonly_indexes {
                readonly.push(*t.addresses.get(usize::from(*i)).context("lookup index")?);
            }
        }
        keys.extend(writable);
        keys.extend(readonly);
    }
    Ok(keys)
}

/// Parses an address lookup table account (owner and the `LookupTable` state tag checked):
/// its 56-byte metadata header, then 32-byte addresses.
pub fn parse_lookup_table(
    key: Pubkey,
    owner: &Pubkey,
    data: &[u8],
) -> anyhow::Result<AddressLookupTableAccount> {
    if *owner != ALT_PROGRAM_ID {
        bail!("{key} is not an address lookup table (owner {owner})");
    }
    let tag = data
        .get(..4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    if tag != Some(1) {
        bail!("{key} is not an initialized lookup table");
    }
    let body = data.get(ALT_META_SIZE..).unwrap_or(&[]);
    if body.len() % 32 != 0 {
        bail!("{key}: truncated lookup table");
    }
    let (chunks, _) = body.as_chunks::<32>();
    let addresses = chunks.iter().map(|c| Pubkey::new_from_array(*c)).collect();
    Ok(AddressLookupTableAccount { key, addresses })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ix(program: Pubkey, accounts: &[(Pubkey, bool, bool)], data: &[u8]) -> Instruction {
        Instruction {
            program_id: program,
            accounts: accounts
                .iter()
                .map(|(k, s, w)| AccountMeta {
                    pubkey: *k,
                    is_signer: *s,
                    is_writable: *w,
                })
                .collect(),
            data: data.to_vec(),
        }
    }

    #[test]
    fn cu_limit_is_ten_percent_over_the_simulation() {
        assert_eq!(cu_limit(0), 1);
        assert_eq!(cu_limit(19_500), 21_450);
        assert_eq!(cu_limit(19_501), 21_452); // ⌈21,451.1⌉
        assert_eq!(cu_limit(10_000_000), MAX_COMPUTE_UNITS);
    }

    #[test]
    fn priority_fee_rounds_up() {
        let b = ComputeBudget {
            unit_limit: 21_450,
            unit_price: 1_000,
        };
        assert_eq!(priority_fee_lamports(&b), 22); // 21.45 → 22
        assert_eq!(
            priority_fee_lamports(&ComputeBudget {
                unit_limit: 1,
                unit_price: 0
            }),
            0
        );
    }

    #[test]
    fn v1_and_v0_round_trip_their_instructions() {
        let payer = Keypair::new();
        let other = Keypair::new();
        let program = Pubkey::new_unique();
        let (a, b) = (Pubkey::new_unique(), Pubkey::new_unique());
        let ixs = vec![
            ix(program, &[(a, false, true), (b, false, false)], &[1, 2, 3]),
            ix(
                program,
                &[(other.pubkey(), true, true), (a, false, true)],
                &[4],
            ),
        ];
        let budget = ComputeBudget {
            unit_limit: 50_000,
            unit_price: 2_000,
        };
        for format in [TxFormat::V1, TxFormat::V0] {
            let tx = build(
                format,
                &payer,
                &[&other],
                &ixs,
                &budget,
                Hash::new_unique(),
                &[],
            )
            .unwrap();
            assert_eq!(format_of(&tx), Some(format));
            assert!(fits(format, &tx));
            let back = decompile(&tx, &[]).unwrap();
            // v0 prepends the two ComputeBudget instructions.
            let back: Vec<_> = back
                .into_iter()
                .filter(|i| i.program_id == program)
                .collect();
            assert_eq!(back, ixs);
        }
    }

    #[test]
    fn a_missing_signer_is_an_error() {
        let payer = Keypair::new();
        let other = Keypair::new();
        let program = Pubkey::new_unique();
        let ixs = vec![ix(program, &[(other.pubkey(), true, false)], &[])];
        let err = build(
            TxFormat::V1,
            &payer,
            &[],
            &ixs,
            &ComputeBudget::SIMULATION,
            Hash::default(),
            &[],
        );
        assert!(err.is_err());
    }

    #[test]
    fn v0_uses_a_lookup_table_and_decompiles_through_it() {
        let payer = Keypair::new();
        let program = Pubkey::new_unique();
        let shared: Vec<Pubkey> = (0..6).map(|_| Pubkey::new_unique()).collect();
        let alt = AddressLookupTableAccount {
            key: Pubkey::new_unique(),
            addresses: shared.clone(),
        };
        let ixs = vec![ix(
            program,
            &shared
                .iter()
                .map(|k| (*k, false, false))
                .collect::<Vec<_>>(),
            &[9],
        )];
        let with = build(
            TxFormat::V0,
            &payer,
            &[],
            &ixs,
            &ComputeBudget::SIMULATION,
            Hash::default(),
            std::slice::from_ref(&alt),
        )
        .unwrap();
        let without = build(
            TxFormat::V0,
            &payer,
            &[],
            &ixs,
            &ComputeBudget::SIMULATION,
            Hash::default(),
            &[],
        )
        .unwrap();
        assert!(size(&with) < size(&without));
        assert_eq!(account_count(&with), account_count(&without));
        let back = decompile(&with, std::slice::from_ref(&alt)).unwrap();
        assert_eq!(back.last(), ixs.last());
        assert!(decompile(&with, &[]).is_err());
    }

    #[test]
    fn lookup_tables_parse() {
        let key = Pubkey::new_unique();
        let a = Pubkey::new_unique();
        let mut data = vec![0u8; ALT_META_SIZE];
        data[0] = 1;
        data.extend_from_slice(a.as_ref());
        let t = parse_lookup_table(key, &ALT_PROGRAM_ID, &data).unwrap();
        assert_eq!(t.addresses, vec![a]);
        assert!(parse_lookup_table(key, &Pubkey::new_unique(), &data).is_err());
        data[0] = 0;
        assert!(parse_lookup_table(key, &ALT_PROGRAM_ID, &data).is_err());
    }
}
