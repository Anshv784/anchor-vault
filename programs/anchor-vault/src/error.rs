use anchor_lang::prelude::*;

#[error_code]
pub enum VaultError {
    #[msg("amount must be more than 0")]
    ZeroAmount,
    #[msg("not enough lamports in the vault")]
    InsufficientFunds,
}
