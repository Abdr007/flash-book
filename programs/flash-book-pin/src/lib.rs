//! Flash Book — Pinocchio port (WIP migration; see docs/QUASAR_MIGRATION_SCOPE.md).
//!
//! Zero-copy, zero-allocation `no_std` program. Accounts are pointer-cast
//! directly from the SVM input buffer — no Borsh, no heap. This eliminates the
//! Anchor framework overhead measured at ~34k CU on `apply_fill` (pilot floor
//! 444 CU vs Anchor 37,779).
//!
//! Status: foundation + `apply_fill` hot path. The remaining instructions are
//! ported incrementally; the matcher/risk math (pure, framework-agnostic) is
//! shared from the Anchor crate's `matcher/` modules as they are de-anchored.
#![no_std]

use pinocchio::{
    account_info::AccountInfo, entrypoint, program_error::ProgramError, pubkey::Pubkey,
    ProgramResult,
};

pub mod state;
pub mod instructions;

entrypoint!(process);
pinocchio::nostd_panic_handler!();

/// Instruction discriminator (1 byte; the Anchor 8-byte sighash is replaced by
/// a compact tag in the V2 wire format). Extend as instructions are ported.
#[repr(u8)]
pub enum Ix {
    ApplyFill = 0,
    // ... 1..=111 ported incrementally (see migration roadmap)
}

#[inline(always)]
fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let (&tag, rest) = data.split_first().ok_or(ProgramError::InvalidInstructionData)?;
    match tag {
        x if x == Ix::ApplyFill as u8 => instructions::apply_fill::process(program_id, accounts, rest),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}
