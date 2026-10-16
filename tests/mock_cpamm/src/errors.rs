//! Mock venue errors. Anchor numbers them from 6000 in declaration order (INTERFACE.md
//! "Errors"); append only.

use anchor_lang::prelude::*;

#[error_code]
#[derive(PartialEq, Eq)]
pub enum MockError {
    #[msg("Pool account data is malformed")]
    BadPoolData,
    #[msg("Unknown layout version (0 or 1)")]
    BadLayoutVersion,
    #[msg("fee_ppm must be below 1,000,000")]
    BadFee,
    #[msg("At most 4 required extras")]
    TooManyExtras,
    #[msg("The quote mint must be WSOL under SPL Token")]
    QuoteNotWsol,
    #[msg("Base and quote mints must differ")]
    SameMint,
    #[msg("Only the pool admin may do this")]
    Unauthorized,
    #[msg("Swaps on this pool are disabled")]
    PoolDisabled,
    #[msg("Vault does not belong to this pool")]
    VaultMismatch,
    #[msg("Mint does not match its vault")]
    MintMismatch,
    #[msg("Token program does not match the mint")]
    TokenProgramMismatch,
    #[msg("Remaining accounts differ from the pool's required extras")]
    ExtrasMismatch,
    #[msg("Authority is neither the owner nor a sufficient delegate of the source")]
    SourceAuthority,
    #[msg("Vault balance is below its reserve counter")]
    InsufficientVault,
    #[msg("Pool reserve is zero")]
    EmptyPool,
    #[msg("Amount must be non-zero")]
    ZeroAmount,
    #[msg("Swap output rounds to zero")]
    ZeroOutput,
    #[msg("Requested output is not below the output reserve")]
    InsufficientLiquidity,
    #[msg("Output below min_out")]
    SlippageMinOut,
    #[msg("Input above max_in")]
    SlippageMaxIn,
    #[msg("Arithmetic overflow")]
    MathOverflow,
    #[msg("Source and destination must not be the pool's own vaults")]
    VaultAsUserAccount,
}
