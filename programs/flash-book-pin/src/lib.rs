//! Flash Book — Pinocchio port (WIP migration; see docs/QUASAR_MIGRATION_SCOPE.md).
//!
//! Zero-copy, zero-allocation `no_std` program (on the SBF target). Accounts are
//! pointer-cast directly from the SVM input buffer — no Borsh, no heap. This
//! eliminates the Anchor framework overhead measured at ~28k CU on apply_fill
//! (Anchor 37,779 → Pinocchio 1,520).
//!
//! The pure fill/settlement math (`fill_math`) is host-unit-tested for exact
//! equivalence with the Anchor implementation; the pinocchio glue
//! (`instructions`, entrypoint) compiles only for the Solana target.
#![cfg_attr(target_os = "solana", no_std)]

pub mod hypertree;
pub mod book;
pub mod state;
pub mod fill_math;
pub mod funding;
pub mod fees;

#[cfg(target_os = "solana")]
mod program {
    use crate::instructions;
    use pinocchio::{
        account_info::AccountInfo, entrypoint, program_error::ProgramError, pubkey::Pubkey,
        ProgramResult,
    };

    entrypoint!(process);

    // Manual panic handler (version-robust across SBF rustc toolchains — the
    // pinocchio 0.8.4 `nostd_panic_handler!` macro uses #[no_mangle], rejected
    // by newer rustc on the panic lang-item).
    #[panic_handler]
    fn panic(_info: &core::panic::PanicInfo) -> ! {
        loop {}
    }

    /// Instruction discriminator (1-byte compact tag; extend as ix are ported).
    #[repr(u8)]
    pub enum Ix {
        ApplyFill = 0,
        SettleFunding = 1,
        PlaceLimitOrder = 2,
        CancelOrder = 3,
        PlaceTakerOrder = 4,
        ModifyOrder = 5,
        CancelAll = 6,
    }

    #[inline(always)]
    fn process(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
        let (&tag, rest) = data.split_first().ok_or(ProgramError::InvalidInstructionData)?;
        match tag {
            x if x == Ix::ApplyFill as u8 => instructions::apply_fill::process(program_id, accounts, rest),
            x if x == Ix::SettleFunding as u8 => instructions::settle_funding::process(program_id, accounts, rest),
            x if x == Ix::PlaceLimitOrder as u8 => instructions::place_order::process(program_id, accounts, rest),
            x if x == Ix::CancelOrder as u8 => instructions::cancel_order::process(program_id, accounts, rest),
            x if x == Ix::PlaceTakerOrder as u8 => instructions::place_taker_order::process(program_id, accounts, rest),
            x if x == Ix::ModifyOrder as u8 => instructions::modify_order::process(program_id, accounts, rest),
            x if x == Ix::CancelAll as u8 => instructions::cancel_all::process(program_id, accounts, rest),
            _ => Err(ProgramError::InvalidInstructionData),
        }
    }
}

#[cfg(target_os = "solana")]
pub mod instructions;
