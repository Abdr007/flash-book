//! process_undelegation — BASE-LAYER callback invoked by the MagicBlock
//! delegation program to finalize an undelegation (after a
//! commit_and_undelegate_* on the ER). Faithful port of the Anchor
//! `process_undelegation`. Re-creates the delegated PDA program-owned and copies
//! the committed buffer back via `er::process_external_undelegate`.
//!
//! DISPATCH: the delegation program CPIs this with the 8-byte
//! `EXTERNAL_UNDELEGATE_DISCRIMINATOR` prefix (NOT pin's 1-byte Ix tag), so the
//! entrypoint detects that prefix and routes here with the seeds as `data`.
//!
//! accounts: [delegated_account (w), buffer (signer, delegation-owned),
//!            payer (signer, w), system_program]
//! data: account_seeds — borsh Vec<Vec<u8>> (the delegated PDA's seeds)

use crate::er::{parse_undelegate_seeds, process_external_undelegate, MAX_UNDELEGATE_SEEDS};
use pinocchio::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, ProgramResult};

pub fn process(pid: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let [delegated_account, buffer, payer, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    let mut seed_slots: [&[u8]; MAX_UNDELEGATE_SEEDS] = [&[]; MAX_UNDELEGATE_SEEDS];
    let count =
        parse_undelegate_seeds(data, &mut seed_slots).ok_or(ProgramError::InvalidInstructionData)?;
    process_external_undelegate(
        pid,
        delegated_account,
        buffer,
        payer,
        system_program,
        &seed_slots[..count],
    )?;

    // AUDIT O-1 (re-audit 2026-06-30): a returning book is the ONLY way a corrupt
    // (malicious-/buggy-ER-committed) book with out-of-range RBT links can reach
    // L1. Validate every node's internal left/right/parent links ONCE, here, after
    // the committed bytes are copied back program-owned — a corrupt book fails
    // CLOSED (the undelegate reverts) instead of panic-DoS'ing the first L1 op and
    // bricking the market. Gated on the book discriminator at ~O(1); the ring /
    // market / outbox are flat arrays covered by their own disc/length checks.
    {
        let d = delegated_account.try_borrow_data()?;
        if d.len() >= 8 && d[..8] == crate::book::MARKET_BOOK_DISC {
            crate::book::MarketBookHandle::validate_node_links(&d)
                .map_err(|_| ProgramError::Custom(255))?; // OutOfRange — corrupt book
        }
    }
    Ok(())
}

// ── entrypoint dispatch change (apply in src/lib.rs `process`, BEFORE the
//    1-byte tag split):
//
//   // The delegation program's undelegation callback arrives with an 8-byte
//   // Anchor-style discriminator, not our 1-byte tag — route it specially.
//   if data.len() >= 8 && data[..8] == crate::er::EXTERNAL_UNDELEGATE_DISCRIMINATOR {
//       return instructions::process_undelegation::process(program_id, accounts, &data[8..]);
//   }
//   let (&tag, rest) = data.split_first().ok_or(ProgramError::InvalidInstructionData)?;
