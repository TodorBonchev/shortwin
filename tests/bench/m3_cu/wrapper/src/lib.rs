//! TEST-ONLY CPI pass-through (the M3 stack-depth test, ARCH §6.3 / C1-8; never deployed).
//!
//! Instruction: accounts `[target program, a₀, a₁, …]`, data `d`. The program invokes `target`
//! with accounts `[a₀, a₁, …]` (each with the signer and writable flags it was given) and data
//! `d`, so it adds exactly one stack level under whatever it forwards — the way a Squads vault
//! transaction or an integrator's program would call short.win. It may forward to itself
//! (direct recursion is allowed by the runtime), which stacks a second level: `[wrapper,
//! wrapper, short.win, …]` runs short.win at stack height 3.
//!
//! On the host the crate is a plain library ([`forward`] only), so the bench workspace builds
//! and lints there too.

use solana_account_info::AccountInfo;
use solana_instruction::{AccountMeta, Instruction};
use solana_program_error::{ProgramError, ProgramResult};
use solana_pubkey::Pubkey;

#[cfg(target_os = "solana")]
solana_program_entrypoint::entrypoint!(process_instruction);

/// The entrypoint's body: forwards `data` and `accounts[1..]` to `accounts[0]`.
///
/// # Errors
/// `NotEnoughAccountKeys` without a target; otherwise whatever the forwarded instruction
/// returns (a failed CPI aborts the transaction).
pub fn process_instruction(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let ix = forward(accounts, data)?;
    solana_cpi::invoke(&ix, accounts)
}

/// The instruction the wrapper forwards: program `accounts[0]`, accounts `accounts[1..]` with
/// their flags, `data`.
///
/// # Errors
/// `NotEnoughAccountKeys` when `accounts` is empty.
pub fn forward(accounts: &[AccountInfo], data: &[u8]) -> Result<Instruction, ProgramError> {
    let (target, rest) = accounts
        .split_first()
        .ok_or(ProgramError::NotEnoughAccountKeys)?;
    Ok(Instruction {
        program_id: *target.key,
        accounts: rest
            .iter()
            .map(|a| AccountMeta {
                pubkey: *a.key,
                is_signer: a.is_signer,
                is_writable: a.is_writable,
            })
            .collect(),
        data: data.to_vec(),
    })
}
