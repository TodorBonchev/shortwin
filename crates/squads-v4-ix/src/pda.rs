//! Squads v4's PDAs (`state/seeds.rs` at `64af733`), all under [`crate::ID`].

use solana_pubkey::Pubkey;

use crate::ID;

const PREFIX: &[u8] = b"multisig";

/// `["multisig", "program_config"]`: the `ProgramConfig` (creation fee and treasury).
pub fn program_config() -> Pubkey {
    Pubkey::find_program_address(&[PREFIX, b"program_config"], &ID).0
}

/// `["multisig", "multisig", create_key]`.
pub fn multisig(create_key: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[PREFIX, b"multisig", create_key.as_ref()], &ID).0
}

/// `["multisig", multisig, "vault", [index]]`: vault `index` of `multisig`, the key that signs
/// what its vault transactions execute.
pub fn vault(multisig: &Pubkey, index: u8) -> Pubkey {
    vault_with_bump(multisig, index).0
}

/// [`vault`] and its bump.
pub fn vault_with_bump(multisig: &Pubkey, index: u8) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[PREFIX, multisig.as_ref(), b"vault", &[index]], &ID)
}

/// `["multisig", multisig, "transaction", index_le]`: a vault, config or batch transaction.
pub fn transaction(multisig: &Pubkey, index: u64) -> Pubkey {
    Pubkey::find_program_address(
        &[
            PREFIX,
            multisig.as_ref(),
            b"transaction",
            &index.to_le_bytes(),
        ],
        &ID,
    )
    .0
}

/// `["multisig", multisig, "transaction", index_le, "proposal"]`.
pub fn proposal(multisig: &Pubkey, index: u64) -> Pubkey {
    Pubkey::find_program_address(
        &[
            PREFIX,
            multisig.as_ref(),
            b"transaction",
            &index.to_le_bytes(),
            b"proposal",
        ],
        &ID,
    )
    .0
}

/// `["multisig", multisig, "transaction_buffer", creator, [buffer_index]]`: a buffer is per
/// creator (only its creator extends it, closes it or turns it into a transaction).
pub fn transaction_buffer(multisig: &Pubkey, creator: &Pubkey, buffer_index: u8) -> Pubkey {
    Pubkey::find_program_address(
        &[
            PREFIX,
            multisig.as_ref(),
            b"transaction_buffer",
            creator.as_ref(),
            &[buffer_index],
        ],
        &ID,
    )
    .0
}

/// `["multisig", transaction, "ephemeral_signer", [index]]`.
pub fn ephemeral_signer(transaction: &Pubkey, index: u8) -> Pubkey {
    Pubkey::find_program_address(
        &[PREFIX, transaction.as_ref(), b"ephemeral_signer", &[index]],
        &ID,
    )
    .0
}
