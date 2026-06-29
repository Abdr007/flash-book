//! commit_market_book / commit_and_undelegate_market_book — ON-THE-ER actions
//! that schedule a commit of the delegated book's state back to the base layer
//! (and, for the `_and_undelegate` variant, also queue undelegation). Faithful
//! ports of the Anchor instructions; both are thin wrappers over `er::cpi_commit`
//! with the same account set. Permissionless (the payer signs).
//!
//! accounts: [payer (signer), market_book (committed, w), magic_context (w),
//!            magic_program]
//! data: (none)

use crate::er::cpi_commit;
use crate::guard::assert_signer;
use pinocchio::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult};

fn run(accounts: &[AccountInfo], allow_undelegation: bool) -> ProgramResult {
    let [payer, market_book, magic_context, magic_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    assert_signer(payer)?;
    cpi_commit(payer, magic_context, magic_program, market_book, allow_undelegation)
}

/// Snapshot the book's state back to base (no undelegation).
pub fn commit(_pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    run(accounts, false)
}

/// Snapshot final state AND queue undelegation — after this lands the delegation
/// program calls back into `process_undelegation` on base to finalize.
pub fn commit_and_undelegate(_pid: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    run(accounts, true)
}
